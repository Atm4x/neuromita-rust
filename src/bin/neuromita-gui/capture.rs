use std::{
    future::{Future, pending},
    pin::Pin,
    sync::Arc,
};

use neuromita::{
    presentation::{CapturePreview, ConversationPresenter},
    video_capture::{CameraDevice, CapturedJpeg, VideoSource},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::runtime::ChatOrigin;

pub enum Action {
    ListCameras,
    Screen(u8),
    Camera(Uuid, usize, u8),
    Discard,
    Save(String),
}

pub struct View {
    pub available: bool,
    pub cameras: Vec<String>,
    pub camera_inventory: Uuid,
    pub busy: bool,
    pub frames: Arc<[CapturedJpeg]>,
    pub status: String,
}

pub enum JobResult {
    Cameras(Result<Vec<CameraDevice>, String>),
    Capture(Result<CapturePreview, String>),
}

type Job = Pin<Box<dyn Future<Output = JobResult> + Send>>;

pub struct State {
    presenter: ConversationPresenter,
    cameras: Vec<CameraDevice>,
    camera_inventory: Uuid,
    preview: Option<CapturePreview>,
    job: Option<Job>,
    status: String,
}

impl State {
    pub fn new(presenter: ConversationPresenter) -> Self {
        Self {
            presenter,
            cameras: Vec::new(),
            camera_inventory: Uuid::new_v4(),
            preview: None,
            job: None,
            status: String::new(),
        }
    }

    pub fn clear(&mut self) {
        self.job = None;
        self.preview = None;
        self.status.clear();
    }

    pub fn view(&self) -> View {
        View {
            available: self.presenter.can_capture_video(),
            cameras: self
                .cameras
                .iter()
                .map(|camera| camera.name.clone())
                .collect(),
            camera_inventory: self.camera_inventory,
            busy: self.job.is_some(),
            frames: self
                .preview
                .as_ref()
                .map(CapturePreview::frames)
                .unwrap_or_default(),
            status: self.status.clone(),
        }
    }

    pub fn action(&mut self, origin: ChatOrigin, action: Action) -> Option<String> {
        match action {
            Action::Discard => self.clear(),
            Action::ListCameras => {
                self.clear();
                self.cameras.clear();
                self.camera_inventory = Uuid::new_v4();
                let presenter = self.presenter.clone();
                self.job = Some(Box::pin(async move {
                    JobResult::Cameras(
                        presenter
                            .list_cameras(&CancellationToken::new())
                            .await
                            .map_err(|err| err.to_string()),
                    )
                }));
            }
            Action::Screen(count) => self.grab(origin, VideoSource::Screen, count),
            Action::Camera(inventory, index, count) if inventory == self.camera_inventory => {
                if let Some(camera) = self.cameras.get(index).cloned() {
                    self.grab(origin, VideoSource::Camera(camera), count);
                }
            }
            Action::Camera(..) => {
                self.status = "Список камер изменился. Выберите источник снова".into()
            }
            Action::Save(text) => {
                if let Some(preview) = self.preview.take() {
                    match self.presenter.commit_capture(&preview, text.clone()) {
                        Ok(_) => {
                            self.status.clear();
                            return Some(text);
                        }
                        Err(error) => self.status = error,
                    }
                }
            }
        }
        None
    }

    fn grab(&mut self, origin: ChatOrigin, source: VideoSource, count: u8) {
        self.clear();
        let presenter = self.presenter.clone();
        self.job = Some(Box::pin(async move {
            JobResult::Capture(
                presenter
                    .prepare_capture(origin.character, source, count, &CancellationToken::new())
                    .await,
            )
        }));
    }

    pub async fn next(&mut self) -> JobResult {
        match self.job.as_mut() {
            Some(job) => job.await,
            None => pending().await,
        }
    }

    pub fn complete(&mut self, result: JobResult) {
        self.job = None;
        match result {
            JobResult::Cameras(Ok(cameras)) => self.cameras = cameras,
            JobResult::Capture(Ok(preview)) => self.preview = Some(preview),
            JobResult::Cameras(Err(error)) | JobResult::Capture(Err(error)) => self.status = error,
        }
    }
}

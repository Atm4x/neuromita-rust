use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use neuromita::{
    character::CharacterId,
    presentation::SandboxSnapshot,
    request::{ProcessingStage, RequestId, RequestOutcome, RequestState},
    timeline::domain::MessageRole,
};
use slint::{ComponentHandle, ModelRc, VecModel};

use super::{
    runtime::{Command, DesktopRuntime, Update},
    view::{AppWindow, ChatRow, ChatStatus},
};

pub fn attach(
    window: &AppWindow,
    directory: PathBuf,
    model: Option<String>,
) -> std::io::Result<DesktopRuntime> {
    window.set_chat_model_input(model.clone().unwrap_or_default().into());
    let weak = window.as_weak();
    let runtime = DesktopRuntime::spawn(directory, model, move |update| {
        let _ = weak.upgrade_in_event_loop(move |window| apply(&window, update));
    })?;
    let weak = window.as_weak();
    let commands = runtime.commands.clone();
    let dispatch = std::rc::Rc::new(move |command| {
        if commands.try_send(command).is_err()
            && let Some(window) = weak.upgrade()
        {
            window.set_chat_sending(false);
            window.set_core_connecting(false);
            window.set_chat_error("Desktop command queue is full or closed".into());
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    window.on_chat_connect(move |model| {
        if let Some(window) = weak.upgrade() {
            window.set_core_connecting(true);
            window.set_chat_error("".into());
        }
        send(Command::Connect(model.into()));
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    window.on_chat_send(move |text| {
        if let Some(window) = weak.upgrade() {
            window.set_chat_sending(true);
        }
        send(Command::Send(text.into()));
    });
    let send = dispatch.clone();
    window.on_chat_select_character(move |id| {
        let character = match id {
            0 => CharacterId::Mita,
            1 => CharacterId::KindMita,
            _ => return,
        };
        send(Command::Select(character));
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    window.on_chat_cancel(move |id| match uuid::Uuid::parse_str(&id) {
        Ok(id) => send(Command::Cancel(RequestId(id))),
        Err(error) => {
            if let Some(window) = weak.upgrade() {
                window.set_chat_error(error.to_string().into());
            }
        }
    });
    let send = dispatch.clone();
    window.on_chat_refresh(move || send(Command::Refresh));
    window.on_chat_set_volume(move |value| {
        if let Ok(value) = u8::try_from(value) {
            dispatch(Command::Volume(value));
        }
    });
    let weak = window.as_weak();
    window.on_chat_dismiss_error(move || {
        if let Some(window) = weak.upgrade() {
            window.set_chat_error("".into());
        }
    });
    Ok(runtime)
}

fn apply(window: &AppWindow, update: Update) {
    match update {
        Update::Stopped => {
            window.set_core_connected(false);
            window.set_core_connecting(false);
            window.set_chat_sending(false);
        }
        Update::Connecting => {
            window.set_core_connecting(true);
            window.set_chat_error("".into());
        }
        Update::Connected(address) => {
            window.set_core_connecting(false);
            window.set_core_connected(true);
            window.set_chat_address(address.into());
        }
        Update::Snapshot(snapshot) => render(window, &snapshot),
        Update::Sent(text) => {
            window.set_chat_sending(false);
            window.set_chat_error("".into());
            if window.get_chat_draft() == text {
                window.set_chat_draft("".into());
            }
        }
        Update::Error(error) => {
            window.set_core_connecting(false);
            window.set_chat_sending(false);
            window.set_chat_error(error.into());
        }
    }
}

fn render(window: &AppWindow, snapshot: &SandboxSnapshot) {
    window.set_chat_character(match snapshot.character {
        CharacterId::Mita => 0,
        CharacterId::KindMita => 1,
    });
    window.set_chat_model(snapshot.model.clone().into());
    window.set_chat_preset(format!("#{}", snapshot.preset.0).into());
    window.set_chat_anchor(
        format!(
            "branch: {}\nthrough: {}",
            snapshot.at.branch_id, snapshot.at.through
        )
        .into(),
    );
    window.set_chat_volume(i32::from(snapshot.settings.values.voice.volume.get()));
    window.set_chat_voice(snapshot.settings.values.voice.in_launcher);
    window.set_chat_attitude(format!("{:.1}", snapshot.state.attitude).into());
    window.set_chat_boredom(format!("{:.1}", snapshot.state.boredom).into());
    window.set_chat_stress(format!("{:.1}", snapshot.state.stress).into());
    window.set_chat_accepted(snapshot.state.accepted_replies.to_string().into());
    window.set_chat_active(
        snapshot
            .requests
            .requests
            .iter()
            .filter(|r| !r.snapshot.state.is_terminal())
            .count() as i32,
    );
    window.set_chat_messages(ModelRc::new(VecModel::from(rows(snapshot))));
    if let Some(error) = &snapshot.requests.discovery_error {
        window.set_chat_error(error.to_string().into());
    }
}

fn rows(snapshot: &SandboxSnapshot) -> Vec<ChatRow> {
    let committed: HashSet<_> = snapshot
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .filter_map(|m| m.request_id)
        .collect();
    let requests = snapshot
        .requests
        .requests
        .iter()
        .filter(|row| row.metadata.character_id == snapshot.character)
        .collect::<Vec<_>>();
    let accepted = requests
        .iter()
        .filter_map(|request| match &request.snapshot.state {
            RequestState::Finished(
                RequestOutcome::Success(response)
                | RequestOutcome::VoiceGenerationFailed { response, .. },
            ) => Some((response.event_id, *request)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut rows = snapshot
        .messages
        .iter()
        .rev()
        .take(200)
        .rev()
        .map(|message| {
            let mut row = ChatRow {
                text: message.content.clone().into(),
                user: message.speaker.as_deref() == Some("player")
                    || (message.speaker.is_none() && message.role == MessageRole::User),
                speaker: message.speaker.as_deref().unwrap_or_default().into(),
                sequence: format!("#{}", message.sequence).into(),
                status: ChatStatus::History,
                ..Default::default()
            };
            if let Some(request) = accepted.get(&message.event_id) {
                row.status = ChatStatus::Success;
                if let RequestState::Finished(RequestOutcome::VoiceGenerationFailed {
                    error, ..
                }) = &request.snapshot.state
                {
                    row.status = ChatStatus::VoiceError;
                    row.detail = error.to_string().into();
                }
            }
            row
        })
        .collect::<Vec<_>>();
    for request in requests {
        if committed.contains(&request.metadata.id) {
            continue;
        }
        let mut row = ChatRow {
            request_id: request.metadata.id.to_string().into(),
            can_cancel: request.snapshot.can_cancel(),
            ..Default::default()
        };
        row.text = request
            .snapshot
            .provisional_text()
            .unwrap_or_default()
            .into();
        row.status = match &request.snapshot.state {
            RequestState::Pending => ChatStatus::Pending,
            RequestState::Processing(ProcessingStage::GeneratingText) => ChatStatus::Text,
            RequestState::Processing(ProcessingStage::TextReady { .. }) => ChatStatus::TextReady,
            RequestState::Processing(ProcessingStage::GeneratingVoice { .. }) => ChatStatus::Voice,
            RequestState::Processing(ProcessingStage::Accepting { .. }) => ChatStatus::Accepting,
            RequestState::Cancelled => ChatStatus::Cancelled,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(error)) => {
                row.detail = error.to_string().into();
                ChatStatus::TextError
            }
            RequestState::Finished(RequestOutcome::InternalError(error)) => {
                row.detail = error.to_string().into();
                row.text = error.partial_text.as_deref().unwrap_or_default().into();
                ChatStatus::InternalError
            }
            RequestState::Finished(
                RequestOutcome::Success(_) | RequestOutcome::VoiceGenerationFailed { .. },
            ) => continue,
        };
        if let Some(error) = &request.observation_error {
            row.detail = error.to_string().into();
        }
        rows.push(row);
    }
    rows
}

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use neuromita::{
    audio::controller::SpeechSessionPhase,
    character::CharacterId,
    presentation::{ConversationHistoryPage, ConversationSnapshot},
    request::{ProcessingStage, RequestId, RequestOutcome, RequestState},
    timeline::domain::MessageRole,
    timeline::{ContextAnchor, domain::ProjectedMessage, selection::TimelineSelection},
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{
    runtime::{ChatOrigin, Command, DesktopRuntime, Update},
    view::{
        AppWindow, CharacterCatalog, CharacterOption, ChatRow, ChatStatus, ConversationUi,
        MemoryUi, SettingsUi,
    },
};

const HISTORY_PAGE_SIZE: usize = 200;

#[derive(Default)]
pub(super) struct HistoryPager {
    owner: Option<ChatOrigin>,
    at: Option<ContextAnchor>,
    selection: Option<TimelineSelection>,
    total: usize,
    visible: usize,
    next_ticket: u64,
    pending: Option<(u64, usize)>,
}

impl HistoryPager {
    #[cfg(test)]
    pub(super) fn visible(&self) -> usize {
        self.visible
    }

    fn reset(&mut self) {
        self.owner = None;
        self.at = None;
        self.selection = None;
        self.total = 0;
        self.visible = 0;
        self.pending = None;
    }

    pub(super) fn observe(
        &mut self,
        owner: ChatOrigin,
        at: ContextAnchor,
        selection: TimelineSelection,
        total: usize,
    ) {
        let same_selection = self.owner == Some(owner)
            && self.selection == Some(selection)
            && self
                .at
                .is_some_and(|previous| previous.branch_id == at.branch_id);
        if !same_selection {
            self.reset();
            self.visible = total.min(HISTORY_PAGE_SIZE);
        } else if self.at != Some(at) || self.total != total {
            self.pending = None;
            self.visible = self
                .visible
                .saturating_add(total.saturating_sub(self.total));
        }
        self.owner = Some(owner);
        self.at = Some(at);
        self.selection = Some(selection);
        self.total = total;
        self.visible = self.visible.min(self.total);
    }

    pub(super) fn request(
        &mut self,
        origin: ChatOrigin,
    ) -> Option<(u64, ContextAnchor, TimelineSelection, usize)> {
        if self.owner != Some(origin) || self.pending.is_some() || self.visible >= self.total {
            return None;
        }
        self.next_ticket = self.next_ticket.checked_add(1)?;
        self.pending = Some((self.next_ticket, self.visible));
        Some((self.next_ticket, self.at?, self.selection?, self.visible))
    }

    fn accept(&mut self, origin: ChatOrigin, ticket: u64, page: &ConversationHistoryPage) -> bool {
        if self.owner != Some(origin)
            || self.at != Some(page.at)
            || self.selection != Some(page.selection)
            || page.character != origin.character
            || self.pending != Some((ticket, page.offset))
            || self.total != page.total
            || page.messages.len() > HISTORY_PAGE_SIZE
            || page.messages.len() > self.total.saturating_sub(page.offset)
        {
            return false;
        }
        self.pending = None;
        self.visible += page.messages.len();
        true
    }

    fn fail(&mut self, origin: ChatOrigin, ticket: u64) -> bool {
        if self.owner != Some(origin) || !self.pending.is_some_and(|(pending, _)| pending == ticket)
        {
            return false;
        }
        self.pending = None;
        true
    }
}

fn pager_status(window: &AppWindow, pager: &HistoryPager) {
    window.set_chat_has_older(pager.at.is_some() && pager.visible < pager.total);
    window.set_chat_older_loading(pager.pending.is_some());
}

pub(super) fn bind_history_actions(
    window: &AppWindow,
    dispatch: Rc<dyn Fn(Command)>,
    revision: Arc<AtomicU64>,
    pager: Arc<Mutex<HistoryPager>>,
) {
    let weak = window.as_weak();
    window.on_chat_load_older(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let Some(origin) = current_origin(&window, &revision) else {
            return;
        };
        let command = pager
            .lock()
            .expect("history pager mutex poisoned")
            .request(origin);
        if let Some((ticket, at, selection, offset)) = command {
            pager_status(
                &window,
                &pager.lock().expect("history pager mutex poisoned"),
            );
            dispatch(Command::LoadOlder(origin, ticket, at, selection, offset));
        }
    });
}

pub fn attach(
    window: &AppWindow,
    directory: PathBuf,
    model: Option<String>,
) -> std::io::Result<DesktopRuntime> {
    let memory_editor =
        std::sync::Arc::new(std::sync::Mutex::new(super::memory::MemoryEditor::default()));
    let editor = memory_editor.clone();
    window.set_chat_model_input(model.clone().unwrap_or_default().into());
    let weak = window.as_weak();
    let selection_revision = Arc::new(AtomicU64::new(0));
    let update_revision = selection_revision.clone();
    let pager = Arc::new(Mutex::new(HistoryPager::default()));
    let update_pager = pager.clone();
    let runtime = DesktopRuntime::spawn(directory, model, move |update| {
        let editor = editor.clone();
        let update_revision = update_revision.clone();
        let update_pager = update_pager.clone();
        let _ = weak.upgrade_in_event_loop(move |window| {
            apply_with_pager(
                &window,
                update,
                &editor,
                &update_revision,
                Some(&update_pager),
            );
        });
    })?;
    let weak = window.as_weak();
    let commands = runtime.commands.clone();
    let queue_editor = memory_editor.clone();
    let queue_pager = pager.clone();
    let dispatch = std::rc::Rc::new(move |command| {
        if let Err(error) = commands.try_send(command)
            && let Some(window) = weak.upgrade()
        {
            match error.into_inner() {
                Command::MemoryEdit(edit_id, ..) => super::memory::failed(
                    &window,
                    &queue_editor,
                    edit_id,
                    "Desktop command queue is full or closed".into(),
                ),
                Command::LoadOlder(origin, ticket, ..) => {
                    let mut pager = queue_pager.lock().expect("history pager mutex poisoned");
                    pager.fail(origin, ticket);
                    pager_status(&window, &pager);
                }
                _ => {}
            }
            window.set_chat_sending(false);
            window.set_core_connecting(false);
            window.set_chat_error("Desktop command queue is full or closed".into());
            window
                .global::<SettingsUi>()
                .set_settings_error("Desktop command queue is full or closed".into());
        }
    });
    super::settings::bind(window, dispatch.clone());
    super::ai_engine::bind(window, dispatch.clone());
    super::voice::bind(window, dispatch.clone());
    super::asr::bind(window, dispatch.clone());
    super::releases::bind(window, dispatch.clone());
    super::memory::bind(window, dispatch.clone(), &memory_editor);
    bind_chat_actions(window, dispatch.clone(), selection_revision.clone());
    bind_history_actions(
        window,
        dispatch.clone(),
        selection_revision.clone(),
        pager.clone(),
    );
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

pub(super) fn bind_chat_actions(
    window: &AppWindow,
    dispatch: std::rc::Rc<dyn Fn(Command)>,
    selection_revision: Arc<AtomicU64>,
) {
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.on_chat_copy_message(move |text| {
        if let Some(window) = weak.upgrade()
            && current_origin(&window, &revision).is_some()
            && !text.is_empty()
        {
            let conversation = window.global::<ConversationUi>();
            conversation.set_copy_text(text);
            conversation.set_copy_open(true);
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.global::<ConversationUi>().on_preview(move |kind| {
        let operation = match kind {
            1 => neuromita::presentation::ConversationChange::ClearMessages,
            2 => neuromita::presentation::ConversationChange::ResetContext,
            _ => return,
        };
        if let Some(window) = weak.upgrade()
            && let Some(origin) = current_origin(&window, &revision)
        {
            send(Command::Preview(origin, operation));
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window
        .global::<ConversationUi>()
        .on_revise(move |target, content| {
            let Some(window) = weak.upgrade() else {
                return;
            };
            match (
                current_origin(&window, &revision),
                uuid::Uuid::parse_str(&target),
            ) {
                (Some(origin), Ok(id)) => send(Command::Preview(
                    origin,
                    neuromita::presentation::ConversationChange::ReviseMessage(
                        neuromita::timeline::EventId(id),
                        content.to_string(),
                    ),
                )),
                (None, _) => window.set_chat_error("Unknown chat character selection".into()),
                (_, Err(error)) => window.set_chat_error(error.to_string().into()),
            }
        });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.global::<ConversationUi>().on_confirm(move || {
        if let Some(window) = weak.upgrade()
            && let Some(origin) = current_origin(&window, &revision)
        {
            send(Command::Confirm(origin));
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.global::<ConversationUi>().on_abort(move || {
        if let Some(window) = weak.upgrade()
            && let Some(origin) = current_origin(&window, &revision)
        {
            send(Command::Abort(origin));
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.global::<ConversationUi>().on_trace(move || {
        if let Some(window) = weak.upgrade()
            && let Some(origin) = current_origin(&window, &revision)
        {
            send(Command::Diagnostics(origin));
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.on_chat_send(move |text| {
        if let Some(window) = weak.upgrade() {
            if let Some(origin) = current_origin(&window, &revision) {
                window.set_chat_sending(true);
                send(Command::Send(origin, text.into()));
            } else {
                window.set_chat_error("Unknown chat character selection".into());
            }
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.on_chat_attach(move |text, path| {
        if let Some(window) = weak.upgrade() {
            if let Some(origin) = current_origin(&window, &revision) {
                window.set_chat_sending(true);
                send(Command::AttachFile(origin, text.into(), path.into()));
            } else {
                window.set_chat_error("Unknown chat character selection".into());
            }
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.on_chat_select_character(move |id| {
        let Some(character) = CharacterId::from_index(id) else {
            return;
        };
        let Some(window) = weak.upgrade() else {
            return;
        };
        let Ok(previous) = revision.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        }) else {
            window.set_chat_error("Chat selection revision exhausted".into());
            return;
        };
        window.set_chat_character(id);
        window.set_chat_has_older(false);
        window.set_chat_older_loading(false);
        reconcile_rows(&window, Vec::new());
        let next = previous + 1;
        let conversation = window.global::<ConversationUi>();
        conversation.set_confirmation(0);
        conversation.set_diagnostics("".into());
        conversation.set_history(false);
        conversation.set_edit_target("".into());
        conversation.set_edit_draft("".into());
        conversation.set_copy_open(false);
        conversation.set_copy_text("".into());
        window.set_chat_sending(false);
        window.set_chat_active_request_id("".into());
        window.set_chat_error("".into());
        send(Command::Select(ChatOrigin {
            character,
            selection_revision: next,
        }));
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.on_chat_hide_message(move |id| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        match (
            current_origin(&window, &revision),
            uuid::Uuid::parse_str(&id),
        ) {
            (Some(origin), Ok(id)) => send(Command::Preview(
                origin,
                neuromita::presentation::ConversationChange::HideMessage(
                    neuromita::timeline::EventId(id),
                ),
            )),
            (None, _) => window.set_chat_error("Unknown chat character selection".into()),
            (_, Err(error)) => window.set_chat_error(error.to_string().into()),
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.on_chat_microphone_toggle(move |enabled| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        match current_origin(&window, &revision) {
            Some(origin) => send(Command::ChatSpeechToggle(origin, enabled)),
            None => window.set_chat_error("Unknown chat character selection".into()),
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.on_chat_retry(move |id| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        match (
            current_origin(&window, &revision),
            uuid::Uuid::parse_str(&id),
        ) {
            (Some(origin), Ok(id)) => send(Command::Retry(origin, RequestId(id))),
            (None, _) => window.set_chat_error("Unknown chat character selection".into()),
            (_, Err(error)) => window.set_chat_error(error.to_string().into()),
        }
    });
    let send = dispatch.clone();
    let weak = window.as_weak();
    let revision = selection_revision.clone();
    window.on_chat_cancel(move |id| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        match (
            current_origin(&window, &revision),
            uuid::Uuid::parse_str(&id),
        ) {
            (Some(origin), Ok(id)) => send(Command::Cancel(origin, RequestId(id))),
            (None, _) => window.set_chat_error("Unknown chat character selection".into()),
            (_, Err(error)) => window.set_chat_error(error.to_string().into()),
        }
    });
}

fn current_origin(window: &AppWindow, revision: &AtomicU64) -> Option<ChatOrigin> {
    Some(ChatOrigin {
        character: CharacterId::from_index(window.get_chat_character())?,
        selection_revision: revision.load(Ordering::Acquire),
    })
}

fn matches_origin(window: &AppWindow, origin: ChatOrigin, revision: &AtomicU64) -> bool {
    super::runtime::chat_fence::accepts(
        origin.character.index() as i32,
        origin.selection_revision,
        window.get_chat_character(),
        revision.load(Ordering::Acquire),
    )
}

pub(super) fn render_catalog(
    window: &AppWindow,
    catalog: Vec<neuromita::character::CharacterDefinition>,
) {
    let entries = catalog
        .into_iter()
        .enumerate()
        .map(|(index, item)| CharacterOption {
            index: index as i32,
            id: item.id.as_str().into(),
            name: item.name.source().into(),
            name_key: item.name.id().into(),
            conversation_name: item.conversation_name.source().into(),
            conversation_key: item.conversation_name.id().into(),
            initial: item.id.as_str().chars().next().unwrap().to_string().into(),
        })
        .collect::<Vec<_>>();
    window
        .global::<CharacterCatalog>()
        .set_entries(ModelRc::new(VecModel::from(entries)));
}

#[cfg(test)]
pub(super) fn apply(
    window: &AppWindow,
    update: Update,
    memory_editor: &super::memory::EditorState,
    revision: &Arc<AtomicU64>,
) {
    apply_with_pager(window, update, memory_editor, revision, None);
}

pub(super) fn apply_with_pager(
    window: &AppWindow,
    update: Update,
    memory_editor: &super::memory::EditorState,
    revision: &Arc<AtomicU64>,
    pager: Option<&Arc<Mutex<HistoryPager>>>,
) {
    match update {
        Update::RecognizedText {
            origin,
            text,
            auto_send_after,
            merge_with_draft,
        } => {
            if !matches_origin(window, origin, revision) {
                return;
            }
            let previous = window.get_chat_draft();
            let combined = if previous.trim().is_empty() {
                text
            } else {
                format!(
                    "{}{}{}",
                    previous,
                    if merge_with_draft { " " } else { "\n" },
                    text
                )
            };
            window.set_chat_draft(combined.clone().into());
            if let Some(delay) = auto_send_after {
                if delay.is_zero() {
                    window.invoke_chat_send(combined.into());
                } else {
                    let weak = window.as_weak();
                    let revision = Arc::clone(revision);
                    slint::Timer::single_shot(delay, move || {
                        if let Some(window) = weak.upgrade()
                            && matches_origin(&window, origin, &revision)
                            && window.get_chat_draft() == combined
                            && !window.get_chat_sending()
                        {
                            window.invoke_chat_send(combined.into());
                        }
                    });
                }
            }
        }
        Update::SpeechSessionStatus(snapshot) => {
            window.set_chat_microphone_phase(match snapshot.phase {
                SpeechSessionPhase::Stopped => 0,
                SpeechSessionPhase::Starting => 1,
                SpeechSessionPhase::Listening => 2,
                SpeechSessionPhase::Stopping => 3,
                SpeechSessionPhase::Failed => 4,
            });
            super::asr::session_status(window, &snapshot);
        }
        Update::MicrophoneTestStatus(snapshot) => {
            let ui = window.global::<super::view::AsrUi>();
            ui.set_testing(snapshot.running);
            ui.set_mic_level(i32::from(snapshot.level));
            ui.set_monitored_device(
                ui.get_device_entries()
                    .iter()
                    .find(|entry| entry.id == snapshot.device.as_str())
                    .map(|entry| entry.caption)
                    .unwrap_or_else(|| snapshot.device.into()),
            );
            if let Some(error) = snapshot.error {
                window
                    .global::<SettingsUi>()
                    .set_settings_error(error.into());
            }
        }
        Update::RuntimeStatus(snapshot) => {
            super::voice::runtime_status(window, &snapshot);
            super::asr::runtime_status(window, &snapshot);
        }
        Update::CharacterCatalog(catalog) => render_catalog(window, catalog),
        Update::SpeechInventory(catalog, devices) => {
            super::voice::inventory(window, &catalog);
            super::asr::inventory(window, &catalog, &devices);
        }
        Update::Releases(snapshot) => super::releases::render(window, &snapshot),
        Update::ReleaseError(error) => window.set_release_error(error.into()),
        Update::Memory(page) => super::memory::render(window, memory_editor, &page),
        Update::MemoryError(error) => window.global::<MemoryUi>().set_error(error.into()),
        Update::MemorySaved(edit_id) => super::memory::saved(window, memory_editor, edit_id),
        Update::MemoryEditError(edit_id, error) => {
            super::memory::failed(window, memory_editor, edit_id, error)
        }
        Update::Confirmation(origin, preview) => {
            if !matches_origin(window, origin, revision) {
                return;
            }
            let ui = window.global::<ConversationUi>();
            match preview {
                Some((operation, count)) => {
                    if matches!(
                        &operation,
                        neuromita::presentation::ConversationChange::ReviseMessage(_, _)
                    ) {
                        ui.set_edit_target("".into());
                        ui.set_edit_draft("".into());
                    }
                    ui.set_count(i32::try_from(count).unwrap_or(i32::MAX));
                    ui.set_confirmation(match operation {
                        neuromita::presentation::ConversationChange::ClearMessages => 1,
                        neuromita::presentation::ConversationChange::ResetContext => 2,
                        neuromita::presentation::ConversationChange::HideMessage(_) => 3,
                        neuromita::presentation::ConversationChange::ReviseMessage(_, _) => 4,
                    });
                }
                None => ui.set_confirmation(0),
            }
        }
        Update::Diagnostics(origin, value) => {
            if !matches_origin(window, origin, revision) {
                return;
            }
            window.global::<ConversationUi>().set_history(false);
            window
                .global::<ConversationUi>()
                .set_diagnostics(value.into());
        }
        Update::Hardware(snapshot) => super::ai_engine::render(window, snapshot),
        Update::Connections(snapshot) => {
            window.set_chat_clients(i32::try_from(snapshot.connected_clients).unwrap_or(i32::MAX));
            window.set_chat_server_running(snapshot.running);
        }
        Update::CharacterDebug {
            character,
            fields,
            error,
        } => {
            super::settings::render_character_debug(window, character, &fields, error.as_deref());
        }
        Update::Administration(snapshot) => {
            super::settings::render(window, &snapshot);
            super::voice::render(window, &snapshot.settings.values.speech.tts);
            super::asr::render(window, &snapshot.settings.values.speech.asr);
        }
        Update::PresetCreated(id, record) => super::settings::created(window, id, &record),
        Update::AdminError(error) => {
            window.global::<SettingsUi>().set_preset_pending_save(false);
            window
                .global::<SettingsUi>()
                .set_settings_error(error.into());
        }
        Update::AdminSaved => super::settings::saved(window, None),
        Update::PresetSaved(id) => super::settings::saved(window, Some(id)),
        Update::Stopped => {
            let stopped = neuromita::ai_runtime::runtime_status::RuntimeSnapshot::default();
            super::voice::runtime_status(window, &stopped);
            super::asr::runtime_status(window, &stopped);
            super::asr::session_status(window, &Default::default());
            window.set_core_connected(false);
            window.set_core_connecting(false);
            window.set_chat_sending(false);
            window.set_chat_active_request_id("".into());
            window.set_chat_microphone_phase(0);
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
        Update::Snapshot(origin, snapshot) => {
            if matches_origin(window, origin, revision) {
                let limit = if let Some(pager) = pager {
                    let mut state = pager.lock().expect("history pager mutex poisoned");
                    state.observe(
                        origin,
                        snapshot.at,
                        snapshot.selection,
                        snapshot.messages.len(),
                    );
                    pager_status(window, &state);
                    state.visible
                } else {
                    HISTORY_PAGE_SIZE
                };
                render(window, &snapshot, limit);
            }
        }
        Update::OlderPage(origin, ticket, result) => {
            let Some(pager) = pager else {
                return;
            };
            if !matches_origin(window, origin, revision) {
                return;
            }
            let mut state = pager.lock().expect("history pager mutex poisoned");
            match result {
                Ok(page) if state.accept(origin, ticket, &page) => {
                    let mut updated = page.messages.iter().map(history_row).collect::<Vec<_>>();
                    let model = window.get_chat_messages();
                    let mut existing = (0..model.row_count())
                        .filter_map(|index| model.row_data(index))
                        .collect::<Vec<_>>();
                    let seen: HashSet<String> = existing
                        .iter()
                        .filter(|row| !row.event_id.is_empty())
                        .map(|row| row.event_id.to_string())
                        .collect();
                    updated.retain(|row| !seen.contains(row.event_id.as_str()));
                    updated.append(&mut existing);
                    reconcile_rows(window, updated);
                }
                Err(error) if state.fail(origin, ticket) => {
                    window.set_chat_error(error.into());
                }
                _ => {}
            }
            pager_status(window, &state);
        }
        Update::Sent(origin, text) => {
            if matches_origin(window, origin, revision) {
                window.set_chat_sending(false);
                window.set_chat_error("".into());
                if window.get_chat_draft() == text {
                    window.set_chat_draft("".into());
                }
            }
        }
        Update::FileAttached(origin, text, path) => {
            if matches_origin(window, origin, revision) {
                window.set_chat_sending(false);
                window.set_chat_error("".into());
                if window.get_chat_draft() == text {
                    window.set_chat_draft("".into());
                }
                if window.get_chat_attachment_path() == path {
                    window.set_chat_attachment_path("".into());
                }
            }
        }
        Update::ChatError(origin, error) => {
            if matches_origin(window, origin, revision) {
                window.set_chat_sending(false);
                window.set_chat_error(error.into());
            }
        }
        Update::ConversationError(origin, error) => {
            if matches_origin(window, origin, revision) {
                window.set_chat_error(error.into());
            }
        }
        Update::Error(error) => {
            if !window.get_core_connected() {
                window
                    .global::<SettingsUi>()
                    .set_settings_error(error.as_str().into());
            }
            window.set_core_connecting(false);
            window.set_chat_sending(false);
            window.set_chat_error(error.into());
        }
    }
}

fn render(window: &AppWindow, snapshot: &ConversationSnapshot, visible: usize) {
    window.set_chat_character(snapshot.character.index() as i32);
    window.set_chat_generation_ready(snapshot.model.is_ok());
    match &snapshot.model {
        Ok(model) => {
            window.set_chat_model(match &snapshot.preset_name {
                Some(name) => format!("{name} ({model})").into(),
                None => model.as_str().into(),
            });
            window.set_chat_generation_error("".into());
        }
        Err(error) => {
            window.set_chat_model("".into());
            window.set_chat_generation_error(error.to_string().into());
        }
    }
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
    let ongoing = snapshot.requests.requests.iter().filter(|request| {
        request.metadata.character_id == snapshot.character && !request.snapshot.state.is_terminal()
    });
    window.set_chat_active(ongoing.clone().count() as i32);
    window.set_chat_active_request_id(
        ongoing
            .filter(|request| request.snapshot.can_cancel())
            .last()
            .map(|request| request.metadata.id.to_string())
            .unwrap_or_default()
            .into(),
    );
    reconcile_rows(window, rows(snapshot, visible));
    if let Some(error) = &snapshot.requests.discovery_error {
        window.set_chat_error(error.to_string().into());
    }
}

fn same_row_identity(current: &ChatRow, next: &ChatRow) -> bool {
    if !current.request_id.is_empty() || !next.request_id.is_empty() {
        return !current.request_id.is_empty() && current.request_id == next.request_id;
    }
    !current.sequence.is_empty()
        && current.sequence == next.sequence
        && current.user == next.user
        && current.speaker == next.speaker
}

pub(super) fn reconcile_rows(window: &AppWindow, next: Vec<ChatRow>) {
    let model = window.get_chat_messages();
    let Some(rows) = model.as_any().downcast_ref::<VecModel<ChatRow>>() else {
        window.set_chat_messages(ModelRc::new(VecModel::from(next)));
        return;
    };
    for (index, desired) in next.iter().enumerate() {
        match rows.row_data(index) {
            Some(current) if same_row_identity(&current, desired) => {
                if current != *desired {
                    rows.set_row_data(index, desired.clone());
                }
            }
            Some(_) => {
                if let Some(later) = ((index + 1)..rows.row_count()).find(|&position| {
                    rows.row_data(position)
                        .is_some_and(|row| same_row_identity(&row, desired))
                }) {
                    for _ in index..later {
                        rows.remove(index);
                    }
                    if rows.row_data(index).is_some_and(|row| row != *desired) {
                        rows.set_row_data(index, desired.clone());
                    }
                } else {
                    rows.insert(index, desired.clone());
                }
            }
            None => rows.push(desired.clone()),
        }
    }
    while rows.row_count() > next.len() {
        rows.remove(next.len());
    }
}

fn history_row(message: &ProjectedMessage) -> ChatRow {
    ChatRow {
        text: message.content.clone().into(),
        attachment_summary: message
            .fields
            .get("content")
            .and_then(|content| neuromita::timeline::domain::parse_message_content(content).ok())
            .map(|parts| {
                parts
                    .into_iter()
                    .filter_map(|part| {
                        part.asset()
                            .map(|asset| format!("{} · {} bytes", asset.media_type, asset.size))
                    })
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .unwrap_or_default()
            .into(),
        user: message.speaker.as_deref() == Some("player")
            || (message.speaker.is_none() && message.role == MessageRole::User),
        speaker: message.speaker.as_deref().unwrap_or_default().into(),
        character_index: message
            .speaker
            .as_deref()
            .and_then(CharacterId::parse)
            .map(|id| id.index() as i32)
            .unwrap_or(-1),
        sequence: format!("#{}", message.sequence).into(),
        event_id: message.event_id.0.to_string().into(),
        can_hide: message.role != MessageRole::System,
        status: ChatStatus::History,
        ..Default::default()
    }
}

fn rows(snapshot: &ConversationSnapshot, visible: usize) -> Vec<ChatRow> {
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
    let latest = requests.last().map(|request| request.metadata.id);
    let latest_visible_player = snapshot
        .messages
        .last()
        .filter(|message| message.kind == neuromita::timeline::domain::EventKind::PlayerMessage)
        .map(|message| message.event_id);
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
        .take(visible)
        .rev()
        .map(|message| {
            let mut row = history_row(message);
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
            character_index: request.metadata.character_id.index() as i32,
            can_cancel: request.snapshot.can_cancel(),
            can_retry: latest == Some(request.metadata.id)
                && request.metadata.source == neuromita::request::RequestSource::Presenter
                && matches!(request.metadata.intent, neuromita::request::TurnIntent::Reply(target) if Some(target) == latest_visible_player)
                && matches!(
                    request.snapshot.state,
                    RequestState::Cancelled
                        | RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
                        | RequestState::Finished(RequestOutcome::InternalError(_))
                ),
            ..Default::default()
        };
        if let Some(tool) = &request.snapshot.tool_activity {
            row.tool_name = tool.name.as_ref().into();
            row.tool_args = tool.args.as_ref().into();
            row.tool_json = tool.json.as_ref().into();
            row.tool_executing = tool.executing;
        }
        row.text = request
            .snapshot
            .provisional_text()
            .or(request.snapshot.streamed_text.as_deref())
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

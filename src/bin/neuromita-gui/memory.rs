use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

use neuromita::{
    character::CharacterMemorySnapshot,
    memory::{MemoryCommand, MemoryId, MemoryPriority},
    presentation::MemoryListPage,
};
use slint::{ComponentHandle, ModelRc, VecModel};

use super::{
    runtime::Command,
    view::{AppWindow, MemoryItem, MemoryUi},
};

#[derive(Default)]
pub struct MemoryEditor {
    current: Option<CharacterMemorySnapshot>,
    selected_character: Option<neuromita::character::CharacterId>,
    draft: Option<CharacterMemorySnapshot>,
    draft_id: Option<uuid::Uuid>,
    offset: usize,
    search: String,
    recall_id: Option<uuid::Uuid>,
    recall_record: Option<MemoryId>,
    graph_id: Option<uuid::Uuid>,
    graph_record: Option<MemoryId>,
    graph_mode: Option<neuromita::knowledge::maintenance::GraphMaintenanceMode>,
}

pub type EditorState = Arc<Mutex<MemoryEditor>>;

pub fn bind(window: &AppWindow, dispatch: Rc<dyn Fn(Command)>, state: &EditorState) {
    let ui = window.global::<MemoryUi>();
    let send = dispatch.clone();
    let selected = state.clone();
    let weak = window.as_weak();
    ui.on_select(move |id| {
        let mut editor = selected.lock().expect("memory editor mutex");
        editor.current = None;
        editor.selected_character = neuromita::character::CharacterId::from_index(id);
        editor.draft = None;
        editor.draft_id = None;
        editor.offset = 0;
        editor.search.clear();
        editor.recall_id = None;
        editor.recall_record = None;
        editor.graph_id = None;
        editor.graph_record = None;
        editor.graph_mode = None;
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<MemoryUi>();
            ui.set_character(id);
            ui.set_editing(false);
            ui.set_edit_id("".into());
            ui.set_content("".into());
            ui.set_error("".into());
            ui.set_delete_confirm(false);
            ui.set_selected_forgotten(false);
            ui.set_recall_query("".into());
            ui.set_recall_result("".into());
            ui.set_recall_pending(false);
            ui.set_graph_eligible(false);
            ui.set_graph_pending(false);
            ui.set_graph_applying(false);
            ui.set_graph_confirm_ready(false);
            ui.set_graph_reindex_confirmation(false);
            ui.set_graph_summary("".into());
            ui.set_search("".into());
            ui.set_offset(0);
            ui.set_next(-1);
            ui.set_active("0".into());
            ui.set_forgotten("0".into());
            ui.set_deleted("0".into());
            ui.set_total("0".into());
            ui.set_items(ModelRc::new(VecModel::from(Vec::<MemoryItem>::new())));
        }
        let Some(character) = neuromita::character::CharacterId::from_index(id) else {
            return;
        };
        send(Command::MemorySelect(character));
    });
    let editor = state.clone();
    let weak = window.as_weak();
    ui.on_page(move |offset, search| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let Ok(offset) = usize::try_from(offset) else {
            return;
        };
        let snapshot = {
            let mut state = editor.lock().expect("memory editor mutex");
            state.offset = offset;
            state.search = search.into();
            state.current.clone()
        };
        if let Some(snapshot) = snapshot {
            match MemoryListPage::from_snapshot(snapshot, Default::default()) {
                Ok(page) => render(&component, &editor, &page),
                Err(error) => component
                    .global::<MemoryUi>()
                    .set_error(error.to_string().into()),
            }
        }
    });
    let editor = state.clone();
    let weak = window.as_weak();
    let abort_send = dispatch.clone();
    ui.on_edit(move |id| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<MemoryUi>();
        let mut editor = editor.lock().expect("memory editor mutex");
        let Some(snapshot) = editor.current.clone() else {
            ui.set_error("Memory snapshot is unavailable".into());
            return;
        };
        editor.recall_id = None;
        editor.recall_record = None;
        if let Some(id) = editor.graph_id.take() {
            abort_send(Command::GraphAbort(id));
        }
        editor.graph_record = None;
        editor.graph_mode = None;
        ui.set_graph_pending(false);
        ui.set_graph_applying(false);
        ui.set_graph_confirm_ready(false);
        ui.set_graph_reindex_confirmation(false);
        ui.set_graph_summary("".into());
        ui.set_graph_eligible(false);
        ui.set_recall_pending(false);
        ui.set_recall_result("".into());
        ui.set_recall_query("".into());
        ui.set_selected_forgotten(false);
        if id.is_empty() {
            ui.set_edit_id("".into());
            ui.set_content("".into());
        } else {
            let Some(record) = snapshot
                .records()
                .iter()
                .find(|record| record.id.0.to_string() == id.as_str())
            else {
                ui.set_error("Memory is no longer available".into());
                return;
            };
            ui.set_selected_forgotten(
                record.kind == neuromita::memory::MemoryKind::Fact
                    && record.state == neuromita::memory::MemoryState::Forgotten,
            );
            ui.set_graph_eligible(
                matches!(
                    record.kind,
                    neuromita::memory::MemoryKind::Fact | neuromita::memory::MemoryKind::RagOnly
                ) && record.state == neuromita::memory::MemoryState::Active,
            );
            ui.set_edit_id(id);
            ui.set_content(record.content.as_str().into());
        }
        editor.draft = Some(snapshot);
        editor.draft_id = Some(uuid::Uuid::new_v4());
        ui.set_editing(true);
        ui.set_error("".into());
    });
    let editor = state.clone();
    let weak = window.as_weak();
    let apply_send = dispatch.clone();
    ui.on_apply(move |action| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<MemoryUi>();
        if !ui.get_editing() {
            return;
        }
        let editor = editor.lock().expect("memory editor mutex");
        let Some(edit_id) = editor.draft_id else {
            return;
        };
        let Some(expected) = editor.draft.clone() else {
            ui.set_error("Select a memory or choose Add first".into());
            return;
        };
        let result = (|| -> Result<MemoryCommand, String> {
            let value = ui.get_edit_id();
            if action == 0 && value.is_empty() {
                return Ok(MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: ui.get_content().into(),
                });
            }
            let id = MemoryId(uuid::Uuid::parse_str(&value).map_err(|e| e.to_string())?);
            let record = expected
                .records()
                .iter()
                .find(|r| r.id == id)
                .ok_or("Memory is absent from the selected snapshot")?;
            Ok(match action {
                0 => MemoryCommand::Update {
                    id,
                    priority: None,
                    content: ui.get_content().into(),
                },
                1 => MemoryCommand::Forget { id },
                2 => MemoryCommand::SoftDelete { id },
                3 => MemoryCommand::Restore { id },
                4 => MemoryCommand::SetProtection {
                    id,
                    protected: !record.protected,
                },
                _ => return Err("Unknown memory operation".into()),
            })
        })();
        match result {
            Ok(command) => {
                drop(editor);
                ui.invoke_graph_cancel();
                ui.set_editing(false);
                apply_send(Command::MemoryEdit(edit_id, expected, vec![command]));
            }
            Err(error) => ui.set_error(error.into()),
        }
    });
    let selected = state.clone();
    let weak = window.as_weak();
    let recall_send = dispatch.clone();
    ui.on_recall(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<MemoryUi>();
        let query = ui.get_recall_query().trim().to_owned();
        if query.is_empty() {
            ui.set_error("Enter a search query for this forgotten memory".into());
            return;
        }
        let id = ui.get_edit_id();
        let mut editor = selected.lock().expect("memory editor mutex");
        let Some(snapshot) = editor.current.as_ref() else {
            ui.set_error("Memory snapshot is unavailable".into());
            return;
        };
        let Some(record) = snapshot
            .records()
            .iter()
            .find(|record| record.id.0.to_string() == id.as_str())
        else {
            ui.set_error("Memory is no longer available".into());
            return;
        };
        if record.kind != neuromita::memory::MemoryKind::Fact
            || record.state != neuromita::memory::MemoryState::Forgotten
        {
            ui.set_error("Only a forgotten fact can be inspected".into());
            return;
        }
        let character = snapshot.character();
        let memory_id = record.id;
        let request_id = uuid::Uuid::new_v4();
        editor.recall_id = Some(request_id);
        editor.recall_record = Some(memory_id);
        drop(editor);
        ui.set_recall_result("".into());
        ui.set_recall_pending(true);
        ui.set_error("".into());
        recall_send(Command::MemoryRecall(
            request_id, character, memory_id, query,
        ));
    });
    let editor = state.clone();
    let weak = window.as_weak();
    let send = dispatch.clone();
    ui.on_graph_preview(move |mode| {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<MemoryUi>();
        if ui.get_graph_pending() || ui.get_graph_confirm_ready() {
            return;
        }
        let mode = match mode {
            0 => neuromita::knowledge::maintenance::GraphMaintenanceMode::Clear,
            1 if ui.get_graph_reindex_available() => {
                neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex
            }
            1 => {
                ui.set_error("Graph Reindex backend is unavailable".into());
                return;
            }
            _ => {
                ui.set_error("Unknown graph maintenance mode".into());
                return;
            }
        };
        let mut editor = editor.lock().expect("memory editor mutex");
        let Some(snapshot) = editor.current.as_ref() else {
            ui.set_error("Memory snapshot is unavailable".into());
            return;
        };
        let Some(record) = snapshot
            .records()
            .iter()
            .find(|r| r.id.0.to_string() == ui.get_edit_id().as_str())
        else {
            ui.set_error("Select a memory source first".into());
            return;
        };
        if record.state != neuromita::memory::MemoryState::Active
            || !matches!(
                record.kind,
                neuromita::memory::MemoryKind::Fact | neuromita::memory::MemoryKind::RagOnly
            )
        {
            ui.set_error("Only active facts or RAG sources can be maintained".into());
            return;
        }
        let character = snapshot.character();
        let id = record.id;
        let request_id = uuid::Uuid::new_v4();
        editor.graph_id = Some(request_id);
        editor.graph_record = Some(id);
        editor.graph_mode = Some(mode);
        drop(editor);
        ui.set_graph_pending(true);
        ui.set_graph_applying(false);
        ui.set_graph_confirm_ready(false);
        ui.set_graph_reindex_confirmation(false);
        ui.set_graph_summary("".into());
        ui.set_error("".into());
        send(Command::GraphPreview(request_id, character, id, mode));
    });
    let editor = state.clone();
    let weak = window.as_weak();
    let send = dispatch.clone();
    ui.on_graph_confirm(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        let ui = component.global::<MemoryUi>();
        if !ui.get_graph_confirm_ready() || ui.get_graph_pending() {
            return;
        }
        let editor = editor.lock().expect("memory editor mutex");
        let (Some(request_id), Some(record), Some(mode), Some(character)) = (
            editor.graph_id,
            editor.graph_record,
            editor.graph_mode,
            editor.current.as_ref().map(|snapshot| snapshot.character()),
        ) else {
            return;
        };
        if ui.get_edit_id().as_str() != record.0.to_string()
            || (mode == neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex
                && !ui.get_graph_reindex_available())
        {
            return;
        }
        drop(editor);
        ui.set_graph_confirm_ready(false);
        ui.set_graph_pending(true);
        ui.set_graph_applying(true);
        ui.set_graph_summary(
            match mode {
                neuromita::knowledge::maintenance::GraphMaintenanceMode::Clear => {
                    "Applying confirmed graph cleanup…"
                }
                neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex => {
                    "Applying confirmed graph reindex…"
                }
            }
            .into(),
        );
        send(Command::GraphApply(request_id, character));
    });
    let editor = state.clone();
    let weak = window.as_weak();
    ui.on_graph_cancel(move || {
        let Some(component) = weak.upgrade() else {
            return;
        };
        if component.global::<MemoryUi>().get_graph_applying() {
            return;
        }
        let id = {
            let mut editor = editor.lock().expect("memory editor mutex");
            editor.graph_record = None;
            editor.graph_mode = None;
            editor.graph_id.take()
        };
        let ui = component.global::<MemoryUi>();
        ui.set_graph_pending(false);
        ui.set_graph_applying(false);
        ui.set_graph_confirm_ready(false);
        ui.set_graph_reindex_confirmation(false);
        ui.set_graph_summary("".into());
        if let Some(id) = id {
            dispatch(Command::GraphAbort(id));
        }
    });
}

pub fn graph_previewed(
    window: &AppWindow,
    state: &EditorState,
    request_id: uuid::Uuid,
    result: Result<usize, String>,
) {
    let mut editor = state.lock().expect("memory editor mutex");
    if editor.graph_id != Some(request_id) {
        return;
    }
    let ui = window.global::<MemoryUi>();
    if editor
        .graph_record
        .is_none_or(|id| ui.get_edit_id().as_str() != id.0.to_string())
    {
        return;
    }
    ui.set_graph_pending(false);
    ui.set_graph_applying(false);
    if editor.graph_mode == Some(neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex)
        && !ui.get_graph_reindex_available()
    {
        editor.graph_id = None;
        editor.graph_record = None;
        editor.graph_mode = None;
        ui.set_graph_confirm_ready(false);
        ui.set_graph_reindex_confirmation(false);
        ui.set_error("Graph Reindex backend is unavailable".into());
        return;
    }
    match result {
        Ok(count) => {
            let reindex = editor.graph_mode
                == Some(neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex);
            ui.set_graph_summary(if reindex {
                format!("Reindex will replace {count} derived graph relations for this source. Confirm to proceed.")
            } else {
                format!("{count} derived graph relations selected for clear. Confirm to proceed.")
            }.into());
            ui.set_graph_reindex_confirmation(reindex);
            ui.set_graph_confirm_ready(true);
        }
        Err(error) => {
            editor.graph_id = None;
            editor.graph_record = None;
            editor.graph_mode = None;
            ui.set_graph_confirm_ready(false);
            ui.set_graph_reindex_confirmation(false);
            ui.set_error(error.into());
        }
    }
}

pub fn graph_applied(
    window: &AppWindow,
    state: &EditorState,
    request_id: uuid::Uuid,
    result: Result<(), String>,
) {
    let mut editor = state.lock().expect("memory editor mutex");
    if editor.graph_id != Some(request_id) {
        return;
    }
    let ui = window.global::<MemoryUi>();
    if editor
        .graph_record
        .is_none_or(|id| ui.get_edit_id().as_str() != id.0.to_string())
    {
        return;
    }
    let mode = editor.graph_mode.take();
    editor.graph_id = None;
    editor.graph_record = None;
    ui.set_graph_pending(false);
    ui.set_graph_applying(false);
    ui.set_graph_confirm_ready(false);
    ui.set_graph_reindex_confirmation(false);
    match result {
        Ok(()) => {
            ui.set_error("".into());
            ui.set_graph_summary(
                match mode {
                    Some(neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex) => {
                        "Graph relations reindexed. Canonical memory retained."
                    }
                    _ => "Graph relations cleared. Canonical memory retained.",
                }
                .into(),
            );
        }
        Err(error) => {
            ui.set_graph_summary("".into());
            ui.set_error(error.into());
        }
    }
}

pub fn render(window: &AppWindow, state: &EditorState, page: &MemoryListPage) {
    let (offset, search) = {
        let mut state = state.lock().expect("memory editor mutex");
        if state
            .selected_character
            .is_some_and(|id| id != page.snapshot().character())
        {
            return;
        }
        let revised = state.current.as_ref().is_some_and(|previous| {
            previous.anchor() != page.snapshot().anchor()
                || previous.records() != page.snapshot().records()
        });
        state.current = Some(page.snapshot().clone());
        let selected_is_still_forgotten = !revised
            && state.recall_record.is_some_and(|id| {
                page.snapshot().records().iter().any(|record| {
                    record.id == id
                        && record.kind == neuromita::memory::MemoryKind::Fact
                        && record.state == neuromita::memory::MemoryState::Forgotten
                })
            });
        if !selected_is_still_forgotten {
            state.recall_id = None;
            state.recall_record = None;
            window.global::<MemoryUi>().set_recall_pending(false);
            window.global::<MemoryUi>().set_recall_result("".into());
        }
        if revised {
            let ui = window.global::<MemoryUi>();
            ui.set_recall_result("".into());
            if !ui.get_graph_applying() {
                let pending = state.graph_id.take().is_some();
                state.graph_record = None;
                state.graph_mode = None;
                ui.set_graph_pending(false);
                ui.set_graph_reindex_confirmation(false);
                ui.set_graph_confirm_ready(false);
                if pending {
                    ui.set_graph_summary("".into());
                }
            }
        }
        let selected_id = window.global::<MemoryUi>().get_edit_id();
        window
            .global::<MemoryUi>()
            .set_selected_forgotten(page.snapshot().records().iter().any(|record| {
                record.id.0.to_string() == selected_id.as_str()
                    && record.kind == neuromita::memory::MemoryKind::Fact
                    && record.state == neuromita::memory::MemoryState::Forgotten
            }));
        window
            .global::<MemoryUi>()
            .set_graph_eligible(page.snapshot().records().iter().any(|record| {
                record.id.0.to_string() == selected_id.as_str()
                    && record.state == neuromita::memory::MemoryState::Active
                    && matches!(
                        record.kind,
                        neuromita::memory::MemoryKind::Fact
                            | neuromita::memory::MemoryKind::RagOnly
                    )
            }));
        (state.offset, state.search.clone())
    };
    let query = neuromita::presentation::MemoryListQuery {
        offset,
        text: Some(search),
        ..Default::default()
    };
    let page = match MemoryListPage::from_snapshot(page.snapshot().clone(), query) {
        Ok(page) => page,
        Err(error) => {
            window
                .global::<MemoryUi>()
                .set_error(error.to_string().into());
            return;
        }
    };
    let ui = window.global::<MemoryUi>();
    ui.set_character(page.snapshot().character().index() as i32);
    ui.set_offset(i32::try_from(offset).unwrap_or(i32::MAX));
    ui.set_next(
        page.next_offset()
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(-1),
    );
    let summary = page.summary();
    ui.set_active(summary.active.to_string().into());
    ui.set_forgotten(summary.forgotten.to_string().into());
    ui.set_deleted(summary.deleted.to_string().into());
    ui.set_total(page.total_matches().to_string().into());
    ui.set_items(ModelRc::new(VecModel::from(
        page.records()
            .iter()
            .map(|record| MemoryItem {
                id: record.id.0.to_string().into(),
                text: record.content.as_str().into(),
                state: match record.state {
                    neuromita::memory::MemoryState::Active => 0,
                    neuromita::memory::MemoryState::Forgotten => 1,
                    neuromita::memory::MemoryState::Deleted => 2,
                },
                priority: match record.priority {
                    MemoryPriority::Low => 0,
                    MemoryPriority::Normal => 1,
                    MemoryPriority::High => 2,
                    MemoryPriority::Critical => 3,
                },
                protected: record.protected,
            })
            .collect::<Vec<_>>(),
    )));
}

pub fn saved(window: &AppWindow, state: &EditorState, edit_id: uuid::Uuid) {
    let mut editor = state.lock().expect("memory editor mutex");
    if editor.draft_id != Some(edit_id) {
        return;
    }
    editor.draft = None;
    editor.draft_id = None;
    let ui = window.global::<MemoryUi>();
    ui.set_error("".into());
    ui.set_edit_id("".into());
    ui.set_content("".into());
    ui.set_editing(false);
}

pub fn failed(window: &AppWindow, state: &EditorState, edit_id: uuid::Uuid, error: String) {
    if state.lock().expect("memory editor mutex").draft_id != Some(edit_id) {
        return;
    }
    let ui = window.global::<MemoryUi>();
    ui.set_editing(true);
    ui.set_error(error.into());
}

pub fn recalled(
    window: &AppWindow,
    state: &EditorState,
    request_id: uuid::Uuid,
    result: Result<Option<String>, String>,
) {
    let mut editor = state.lock().expect("memory editor mutex");
    if editor.recall_id != Some(request_id) {
        return;
    }
    editor.recall_id = None;
    let ui = window.global::<MemoryUi>();
    if editor
        .recall_record
        .is_none_or(|id| ui.get_edit_id().as_str() != id.0.to_string())
    {
        return;
    }
    ui.set_recall_pending(false);
    match result {
        Ok(Some(text)) => ui.set_recall_result(text.into()),
        Ok(None) => ui.set_recall_result("No matching forgotten memory".into()),
        Err(error) => ui.set_error(error.into()),
    }
}

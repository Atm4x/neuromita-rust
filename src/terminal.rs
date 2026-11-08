use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::Arc,
};

use neuromita::{
    api::{
        ChatApi, GraphMaintenanceApi, HistoricalRecallApi, HistorySearchApi, KnowledgeCacheApi,
        MemoryTransferApi, PortableTransferApi, VariableTransferApi,
    },
    app::ApplicationLifecycle,
    character::CharacterId,
    games::{
        GameApi, GameReceipt,
        seabattle::{Coordinate, Orientation, Side},
    },
    knowledge::{
        forgotten_recall::RecallSelector,
        maintenance::{GraphMaintenanceMode, GraphMaintenancePreview},
        memory_transfer::{MAX_ARCHIVE_BYTES, MemoryImportPreview},
        portable_transfer::{MAX_PORTABLE_ARCHIVE_BYTES, PortableImportPreview},
        retrieval_fusion::FusionMode,
        semantic_memory::EmbeddingCachePreview,
        variable_transfer::VariableImportPreview,
    },
    memory::MemoryId,
    presentation::ChatPresenter,
    request::{
        DiagnosticScope, DiagnosticSection, RequestId, RequestOutcome, RequestSnapshot,
        RequestSource, RequestState,
    },
    settings::{HistoryGenerationMode, HistoryGenerationSettings, SettingsApi},
};
use tokio::sync::mpsc;

mod ai_worker;

pub struct Options {
    pub model: String,
    character: CharacterId,
    message: Option<String>,
    pub listen: std::net::SocketAddr,
    pub data_dir: PathBuf,
    pub finetune_config: Option<PathBuf>,
    pub installed_speech: bool,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl Options {
    pub fn parse() -> io::Result<Option<Self>> {
        Self::parse_args(std::env::args().skip(1))
    }

    fn parse_args(args: impl IntoIterator<Item = String>) -> io::Result<Option<Self>> {
        let mut args = args.into_iter();
        let mut values = HashMap::new();
        let mut installed_speech = false;
        while let Some(argument) = args.next() {
            if argument == "--installed-speech" {
                if installed_speech {
                    return Err(invalid("duplicate --installed-speech"));
                }
                installed_speech = true;
                continue;
            }
            if argument == "--help" {
                println!(
                    "neuromita --model MODEL --character Crazy|Kind --listen HOST:PORT [--message TEXT] [--data-dir PATH] [--finetune-config PATH] [--installed-speech]\n\n--installed-speech: opt in to previously installed TTS/ASR selections from persistent settings; no model installs or downloads.\nRequired environment: GEMINI_API_KEY\nNetwork command: test followed by newline\nInteractive commands: /ai worker status, /ai worker restart Embedding|Reranker|Tts|Asr EXPECTED_GENERATION_UUID, /cancel REQUEST_UUID, /character Crazy|Kind, /recall forgotten id UUID | QUERY, /recall forgotten search | QUERY, /rag history QUERY, /rag history-fts QUERY, /rag history-vector QUERY, /rag history-fusion QUERY, /rag generation [off|keyword|fts|vector|fusion|window N], /knowledge cache preview|clear TOKEN, /knowledge graph inspect MEMORY_UUID..., /knowledge graph preview clear|reindex MEMORY_UUID..., /knowledge graph apply TOKEN, /knowledge memory export PATH|preview PATH|apply TOKEN, /knowledge variables export PATH|preview PATH|apply TOKEN, /knowledge portable export PATH|preview PATH|apply TOKEN, /diagnostics ..., /dataset status, /game start|place|random|fire|turn|auto|close|view, /reminders, /reminder add|timer|update|delete|enable|disable|retry|zone|now ..., /quit\n\n{}",
                    crate::settings_cli::HELP
                );
                return Ok(None);
            }
            if !matches!(
                argument.as_str(),
                "--model"
                    | "--character"
                    | "--message"
                    | "--listen"
                    | "--data-dir"
                    | "--finetune-config"
            ) {
                return Err(invalid("unknown argument; use --help"));
            }
            let value = args
                .next()
                .ok_or_else(|| invalid("argument requires a value"))?;
            if values.insert(argument, value).is_some() {
                return Err(invalid("duplicate argument"));
            }
        }
        let model = values
            .remove("--model")
            .ok_or_else(|| invalid("--model is required"))?;
        let character = values
            .remove("--character")
            .ok_or_else(|| invalid("--character is required"))?;
        let character = CharacterId::parse(&character)
            .ok_or_else(|| invalid("unknown character; use a canonical character ID"))?;
        let listen = values
            .remove("--listen")
            .ok_or_else(|| invalid("--listen is required"))?
            .parse()
            .map_err(|_| invalid("invalid listen address"))?;
        Ok(Some(Self {
            model,
            installed_speech,
            character,
            message: values.remove("--message"),
            listen,
            data_dir: match values.remove("--data-dir") {
                Some(path) if path.is_empty() => return Err(invalid("empty data directory")),
                Some(path) => PathBuf::from(path),
                None => default_data_dir()?,
            },
            finetune_config: match values.remove("--finetune-config") {
                Some(path) if path.is_empty() => {
                    return Err(invalid("empty fine-tune configuration path"));
                }
                Some(path) => Some(PathBuf::from(path)),
                None => None,
            },
        }))
    }
}

fn default_data_dir() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("NEUROMITA_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(path).join("NeuroMita"));
    }
    #[cfg(not(windows))]
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("neuromita"));
    }
    if let Some(path) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(path).join(".config").join("neuromita"));
    }
    Err(invalid("data directory unavailable; specify --data-dir"))
}

fn input() -> mpsc::Receiver<io::Result<String>> {
    let (send, receive) = mpsc::channel(16);
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if send.blocking_send(line).is_err() {
                break;
            }
        }
    });
    receive
}

/// Real user workflow over the same typed `RemindersApi` the model tools use.
async fn reminder_command(character: CharacterId, line: &str) -> Result<String, String> {
    use neuromita::reminders::{
        CreateReminder, CreateTimer, RemindersApi, TimeZoneId, UpdateReminder, parse_friendly_due,
        parse_structured_repeat, system_timezone,
    };
    let settings =
        neuromita::settings::SettingsApi::snapshot::<neuromita::settings::RemindersSettings>()
            .map_err(|error| error.to_string())?;
    let zone = settings.value.timezone.clone();
    let policy = settings.value.policy;
    let now = RemindersApi::now()
        .await
        .map_err(|error| error.to_string())?;
    let rest = if line.trim() == "/reminders" {
        "list"
    } else {
        line.trim_start_matches("/reminder").trim()
    };
    let (action, args) = match rest.split_once(char::is_whitespace) {
        Some((action, args)) => (action, args.trim()),
        None => (rest, ""),
    };
    match action {
        "" | "list" => {
            let views = RemindersApi::list(character)
                .await
                .map_err(|error| error.to_string())?;
            let mut lines = views.iter().map(|view| view.summary()).collect::<Vec<_>>();
            if lines.is_empty() {
                lines.push("Нет активных напоминаний.".into());
            }
            let snapshot = RemindersApi::snapshot().map_err(|error| error.to_string())?;
            for delivery in snapshot
                .deliveries
                .iter()
                .filter(|delivery| delivery.character == character)
            {
                lines.push(format!(
                    "Доставка {}: {:?}; запрос {:?}; ошибка {:?}",
                    delivery.id, delivery.state, delivery.request_id, delivery.last_error
                ));
            }
            Ok(lines.join("\n"))
        }
        "add" => {
            let (due, text) = args
                .split_once('|')
                .ok_or("формат: /reminder add <когда> | <текст>")?;
            let due = parse_friendly_due(due.trim(), now, &zone, policy)
                .map_err(|error| error.to_string())?;
            let view = RemindersApi::create(CreateReminder::new(character, text.trim(), due))
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!("Создано: {}", view.summary()))
        }
        "timer" => {
            let (delay, instruction) = args
                .split_once('|')
                .ok_or("формат: /reminder timer <секунды> | <инструкция>")?;
            let delay: f64 = delay
                .trim()
                .parse()
                .map_err(|_| "задержка должна быть числом секунд".to_string())?;
            if !delay.is_finite() || delay <= 0.0 {
                return Err("задержка должна быть положительной".into());
            }
            let delay = std::time::Duration::try_from_secs_f64(delay)
                .map_err(|_| "задержка вне допустимого диапазона".to_string())?;
            let view =
                RemindersApi::create_timer(CreateTimer::new(character, instruction.trim(), delay))
                    .await
                    .map_err(|error| error.to_string())?;
            Ok(format!("Запущен таймер: {}", view.summary()))
        }
        "update" => {
            let (number, rest) = args
                .split_once(char::is_whitespace)
                .ok_or("формат: /reminder update <N> text|due|repeat <значение>")?;
            let number: u64 = number
                .parse()
                .map_err(|_| "N должно быть числом".to_string())?;
            let (field, value) = rest
                .split_once(char::is_whitespace)
                .ok_or("формат: /reminder update <N> text|due|repeat <значение>")?;
            let mut input = UpdateReminder::new(character, number);
            match field {
                "text" => input = input.text(value.trim()),
                "due" => {
                    let due = parse_friendly_due(value.trim(), now, &zone, policy)
                        .map_err(|error| error.to_string())?;
                    input = input.due(due);
                }
                "repeat" => {
                    let repeat =
                        parse_structured_repeat(value.trim()).map_err(|error| error.to_string())?;
                    input = input.repeat(repeat);
                }
                other => return Err(format!("неизвестное поле '{other}'")),
            }
            let view = RemindersApi::update(input)
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!("Обновлено: {}", view.summary()))
        }
        "delete" => {
            let number: u64 = args
                .parse()
                .map_err(|_| "формат: /reminder delete <N>".to_string())?;
            let view = RemindersApi::delete(character, number)
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!("Удалено: {}", view.summary()))
        }
        "enable" | "disable" => {
            RemindersApi::set_enabled(action == "enable")
                .await
                .map_err(|error| error.to_string())?;
            Ok(if action == "enable" {
                "Планировщик напоминаний включён".into()
            } else {
                "Планировщик напоминаний выключен; записи сохранены".into()
            })
        }
        "retry" => {
            let id = args
                .parse::<uuid::Uuid>()
                .map_err(|_| "формат: /reminder retry <delivery-uuid>".to_string())?;
            let delivery = neuromita::reminders::DeliveryId(neuromita::timeline::EventId(id));
            RemindersApi::retry(delivery)
                .await
                .map_err(|error| error.to_string())?;
            Ok("Повтор доставки разрешён".into())
        }
        "zone" => {
            let zone = if args.eq_ignore_ascii_case("local") {
                system_timezone().map_err(|error| error.to_string())?
            } else {
                TimeZoneId::parse(args).map_err(|error| error.to_string())?
            };
            RemindersApi::set_timezone(zone.clone())
                .await
                .map_err(|error| error.to_string())?;
            Ok(format!("Часовой пояс напоминаний: {zone}"))
        }
        "now" => Ok(format!("Сейчас: {now}")),
        other => Err(format!("неизвестная команда /reminder {other}")),
    }
}

fn display(snapshot: &RequestSnapshot) {
    let id = snapshot.metadata.id;
    let character = snapshot.metadata.character_id;
    match &snapshot.state {
        RequestState::Pending => println!("[{id}] {character}: pending"),
        RequestState::Processing(_) => println!("[{id}] {character}: processing"),
        RequestState::Cancelled => println!("[{id}] {character}: cancelled"),
        RequestState::Finished(RequestOutcome::Success(response)) => {
            println!("[{id}] {character}: success\n{}", response.text)
        }
        RequestState::Finished(RequestOutcome::TextGenerationFailed(error)) => {
            eprintln!("[{id}] {character}: text generation failed: {error}")
        }
        RequestState::Finished(RequestOutcome::InternalError(error)) => {
            eprintln!("[{id}] {character}: internal error: {error}")
        }
        RequestState::Finished(RequestOutcome::VoiceGenerationFailed { response, error }) => {
            eprintln!(
                "[{id}] {character}: voice failed: {error}\n{}",
                response.text
            );
        }
    }
    if snapshot.state.is_terminal() {
        if let Some(sample_id) = snapshot.dataset.sample_id {
            println!("[{id}] dataset sample: {sample_id}");
        }
        if let Some(warning) = &snapshot.dataset.warning {
            eprintln!("[{id}] dataset warning: {warning}");
        }
    }
}

fn dataset_status(dataset: Option<&neuromita::finetune::FineTuneCollector>) -> String {
    let Some(dataset) = dataset else {
        return "Dataset collection is not configured".into();
    };
    let status = dataset.delivery_status();
    format!(
        "Dataset enabled={} closed={} admitted={} completed={} pending={} dropped={} rejected={} errors={}\nLast delivery error: {}",
        dataset.enabled(),
        status.closed,
        status.admitted,
        status.completed,
        status.pending,
        status.dropped,
        status.rejected,
        status.errors,
        status.last_error.as_deref().unwrap_or("none")
    )
}

pub async fn run(
    app: &ApplicationLifecycle,
    options: Options,
    dataset: Option<&neuromita::finetune::FineTuneCollector>,
) -> io::Result<()> {
    let presenter: &ChatPresenter = &app.presenter().chat;
    let mut character = options.character;
    let mut updates = presenter.observe();
    let mut printed: HashMap<RequestId, u64> = HashMap::new();
    let mut pending = HashSet::new();
    let mut failed = false;
    let mut pending_cache_clear: Option<(uuid::Uuid, EmbeddingCachePreview)> = None;
    let mut pending_graph: Option<(uuid::Uuid, CharacterId, GraphMaintenancePreview)> = None;
    let mut pending_memory_import: Option<(uuid::Uuid, CharacterId, MemoryImportPreview)> = None;
    let mut pending_variable_import: Option<(uuid::Uuid, CharacterId, VariableImportPreview)> =
        None;
    let mut pending_portable_import: Option<(uuid::Uuid, CharacterId, PortableImportPreview)> =
        None;
    let mut eof = options.message.is_some();
    let mut lines = if eof { None } else { Some(input()) };
    if let Some(message) = options.message {
        let handle = presenter
            .reply(character, message)
            .map_err(|error| invalid(&error.to_string()))?;
        pending.insert(handle.id());
    } else {
        println!(
            "Gemini model: {}. Character: {character}. Enter a message; /quit to exit.",
            options.model
        );
    }
    loop {
        let model = { Arc::clone(&updates.borrow_and_update()) };
        if let Some(error) = &model.discovery_error {
            eprintln!("Observation warning: {error}");
        }
        for row in &model.requests {
            let id = row.metadata.id;
            if printed
                .get(&id)
                .is_none_or(|revision| *revision < row.snapshot.revision)
            {
                display(&row.snapshot);
                printed.insert(id, row.snapshot.revision);
            }
            if row.snapshot.state.is_terminal()
                && pending.remove(&id)
                && !matches!(
                    row.snapshot.state,
                    RequestState::Finished(RequestOutcome::Success(_))
                )
            {
                failed = true;
            }
            if let Some(error) = &row.observation_error {
                return Err(io::Error::other(error.to_string()));
            }
        }
        let observed_active = model
            .requests
            .iter()
            .any(|row| !row.snapshot.state.is_terminal());
        if eof && pending.is_empty() && !observed_active {
            return if failed {
                Err(io::Error::other("one or more requests did not succeed"))
            } else {
                Ok(())
            };
        }
        tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal?; return Ok(()); }
            changed = updates.changed() => {
                changed.map_err(|_| io::Error::other("presenter stopped"))?;
            }
            line = async { lines.as_mut().expect("input enabled").recv().await }, if lines.is_some() => {
                match line {
                    None => { eof = true; lines = None; }
                    Some(Err(error)) => return Err(error),
                    Some(Ok(line)) => {
                        if line == "/quit" { return Ok(()); }
                        if let Some(value) = line.strip_prefix("/character ") {
                            match CharacterId::parse(value) {
                                Some(id) => { character = id; pending_graph = None; pending_memory_import = None; pending_variable_import = None; pending_portable_import = None; println!("Character: {id}"); }
                                None => eprintln!("Unknown character"),
                            }
                        } else if slash_command(&line, "/ai worker") {
                            match ai_worker::run(app, &line).await {
                                Ok(output) => println!("{output}"),
                                Err(error) => eprintln!("AI worker: {error}"),
                            }
                        } else if let Some(value) = line.strip_prefix("/cancel ") {
                            match uuid::Uuid::parse_str(value) {
                                Ok(id) => match presenter.cancel(RequestId(id)) {
                                    Ok(result) => println!("Cancel: {result:?}"),
                                    Err(error) => eprintln!("{error}"),
                                }
                                Err(_) => eprintln!("Invalid request UUID"),
                            }
                        } else if slash_command(&line, "/recall") {
                            match parse_forgotten_recall(&line) {
                                Ok((selector, query)) => {
                                    match HistoricalRecallApi::inspect(character, selector, &query).await {
                                        Ok(Some(text)) => println!("{text}"),
                                        Ok(None) => println!("Нет совпадающих забытых воспоминаний."),
                                        Err(error) => eprintln!("Recall: {error}"),
                                    }
                                }
                                Err(error) => eprintln!("{error}"),
                            }
                        } else if slash_command(&line, "/rag generation") {
                            match history_generation_command(&line) {
                                Ok(text) => println!("{text}"),
                                Err(error) => eprintln!("History policy: {error}"),
                            }
                        } else if slash_command(&line, "/rag") {
                            match parse_history_search(&line) {
                                Ok(selection) => {
                                    let result = match selection {
                                        HistoryCommand::Keyword(query) => HistorySearchApi::inspect(character, query).await,
                                        HistoryCommand::Fts(query) => HistorySearchApi::inspect_fts(character, query).await,
                                        HistoryCommand::Vector(query) => HistorySearchApi::inspect_semantic(character, query, FusionMode::VectorOnly, None).await,
                                        HistoryCommand::Fusion(query) => HistorySearchApi::inspect_semantic(character, query, FusionMode::Union, None).await,
                                    };
                                    match result {
                                    Ok(result) => {
                                        if result.hits.is_empty() {
                                            println!("История: {:?}.", result.status);
                                        }
                                        for hit in result.hits {
                                            println!("[HISTORY {}] {}", hit.key.id, hit.content);
                                        }
                                    }
                                    Err(error) => eprintln!("History: {error}"),
                                    }
                                },
                                Err(error) => eprintln!("{error}"),
                            }
                        } else if slash_command(&line, "/knowledge memory") {
                            match parse_memory_transfer(&line) {
                                Ok(MemoryTransferCommand::Export(path)) => {
                                    match MemoryTransferApi::export(character).await {
                                        Ok(bytes) => match write_no_clobber_export(&path, &bytes) {
                                            Ok(ExportPublication::Durable) => println!("Memory records for {character}: {} bytes durably published to {path}. Scope: retained MemoryRecord only; NOT history, variables, or graph.", bytes.len()),
                                            Ok(ExportPublication::PublishedDurabilityUnconfirmed(warning)) => eprintln!("Memory export: COMPLETE bytes published to {path} but durability/cleanup is unconfirmed: {warning}. Check the file before retrying; it was not overwritten."),
                                            Err(error) => eprintln!("Memory export not published: {error}"),
                                        },
                                        Err(error) => eprintln!("Memory export: {error}"),
                                    }
                                }
                                Ok(MemoryTransferCommand::Preview(path)) => {
                                    pending_memory_import = None;
                                    match read_scoped_archive(&path, MAX_ARCHIVE_BYTES) {
                                        Ok(bytes) => match MemoryTransferApi::preview_import(character, &bytes).await {
                                            Ok(plan) => {
                                                let token = uuid::Uuid::new_v4();
                                                println!("Memory import preview: source={}, explicit target={}, incoming={}, new={}, identical={}, branch/head={:?}. Only retained MemoryRecord. To confirm: /knowledge memory apply {token}",
                                                    plan.source_character(), plan.target_character(), plan.incoming_records(), plan.new_records(), plan.identical_records(), plan.target_anchor());
                                                pending_memory_import = Some((token, character, plan));
                                            }
                                            Err(error) => eprintln!("Memory import preview: {error}"),
                                        },
                                        Err(error) => eprintln!("Memory import preview: {error}"),
                                    }
                                }
                                Ok(MemoryTransferCommand::Apply(token)) => {
                                    match take_scoped_preview(&mut pending_memory_import, character, token, "Memory") {
                                        Ok(plan) => match MemoryTransferApi::apply_confirmed(
                                            character, plan, true, &tokio_util::sync::CancellationToken::new()
                                        ).await {
                                            Ok(result) => println!("Memory import: atomically inserted {} exact records; canonical records retained, unchanged duplicates not rewritten.", result.applied.len()),
                                            Err(error) => eprintln!("Memory import: {error}; request a new preview."),
                                        },
                                        Err(error) => eprintln!("Memory import: {error}"),
                                    }
                                }
                                Err(error) => eprintln!("Memory transfer: {error}"),
                            }
                        } else if slash_command(&line, "/knowledge variables") {
                            match parse_variable_transfer(&line) {
                                Ok(MemoryTransferCommand::Export(path)) => {
                                    match VariableTransferApi::export(character).await {
                                        Ok(bytes) => match write_no_clobber_export(&path, &bytes) {
                                            Ok(ExportPublication::Durable) => println!("User variables: {} bytes exported to {path}; only canonical character variables, not computed state or history.", bytes.len()),
                                            Ok(ExportPublication::PublishedDurabilityUnconfirmed(warning)) => eprintln!("Variable export published to {path}, but durability/cleanup uncertain: {warning}"),
                                            Err(error) => eprintln!("Variable export not published: {error}"),
                                        },
                                        Err(error) => eprintln!("Variable export: {error}"),
                                    }
                                }
                                Ok(MemoryTransferCommand::Preview(path)) => {
                                    pending_variable_import = None;
                                    match read_scoped_archive(&path, neuromita::knowledge::variable_transfer::MAX_VARIABLE_ARCHIVE_BYTES) {
                                        Ok(bytes) => match VariableTransferApi::preview_import(character, &bytes).await {
                                            Ok(plan) => {
                                                let token = uuid::Uuid::new_v4();
                                                println!("Variable import: source={}, target={}, entries={}, new={}, identical={}, branch/head={:?}; only canonical variables. Confirm with /knowledge variables apply {token}",
                                                    plan.source_character(), plan.target_character(), plan.incoming_count(), plan.added_count(), plan.identical_count(), plan.target_anchor());
                                                pending_variable_import = Some((token, character, plan));
                                            }
                                            Err(error) => eprintln!("Variable import preview: {error}"),
                                        },
                                        Err(error) => eprintln!("Variable import preview: {error}"),
                                    }
                                }
                                Ok(MemoryTransferCommand::Apply(token)) => {
                                    match take_scoped_preview(&mut pending_variable_import, character, token, "Variables") {
                                        Ok(plan) => match VariableTransferApi::apply_confirmed(
                                            character, plan, true, &tokio_util::sync::CancellationToken::new()
                                        ).await {
                                            Ok(snapshot) => println!("Variable import committed; character now has {} user variables, computed state untouched.", snapshot.values().len()),
                                            Err(error) => eprintln!("Variable import: {error}; request new preview."),
                                        },
                                        Err(error) => eprintln!("Variable import: {error}"),
                                    }
                                }
                                Err(error) => eprintln!("Variable transfer: {error}"),
                            }
                        } else if slash_command(&line, "/knowledge portable") {
                            match parse_portable_transfer(&line) {
                                Ok(MemoryTransferCommand::Export(path)) => {
                                    match PortableTransferApi::export(character).await {
                                        Ok(bytes) => match write_no_clobber_export(&path, &bytes) {
                                            Ok(ExportPublication::Durable) => println!("Portable archive: {} bytes published to {path}. Scope: retained MemoryRecord + canonical user variables; NOT History/SessionArchive.", bytes.len()),
                                            Ok(ExportPublication::PublishedDurabilityUnconfirmed(warning)) => eprintln!("Portable archive published to {path}, but durability uncertain: {warning}"),
                                            Err(error) => eprintln!("Portable export not published: {error}"),
                                        },
                                        Err(error) => eprintln!("Portable export: {error}"),
                                    }
                                }
                                Ok(MemoryTransferCommand::Preview(path)) => {
                                    pending_portable_import = None;
                                    match read_scoped_archive(&path, MAX_PORTABLE_ARCHIVE_BYTES) {
                                        Ok(bytes) => match PortableTransferApi::preview_import(character, &bytes).await {
                                            Ok(plan) => {
                                                let token = uuid::Uuid::new_v4();
                                                println!("Portable import: source={}, target={}, memory={} incoming/{} new, variables={} incoming/{} new, anchor={:?}. A SINGLE atomic guarded transaction, excluding History. Confirm with /knowledge portable apply {token}",
                                                    plan.source_character(), plan.target_character(), plan.incoming_memories(), plan.new_memories(), plan.incoming_variables(), plan.new_variables(), plan.target_anchor());
                                                pending_portable_import = Some((token, character, plan));
                                            }
                                            Err(error) => eprintln!("Portable import preview: {error}"),
                                        },
                                        Err(error) => eprintln!("Portable import preview: {error}"),
                                    }
                                }
                                Ok(MemoryTransferCommand::Apply(token)) => {
                                    match take_scoped_preview(&mut pending_portable_import, character, token, "Portable") {
                                        Ok(plan) => match PortableTransferApi::apply_confirmed(
                                            character, plan, true, &tokio_util::sync::CancellationToken::new()
                                        ).await {
                                            Ok(result) => println!("Portable import: atomically inserted {} memories and {} variables; History unchanged.", result.applied_memories, result.applied_variables),
                                            Err(error) => eprintln!("Portable import: {error}; request a new preview."),
                                        },
                                        Err(error) => eprintln!("Portable import: {error}"),
                                    }
                                }
                                Err(error) => eprintln!("Portable transfer: {error}"),
                            }
                        } else if slash_command(&line, "/knowledge graph") {
                            match parse_graph_maintenance(&line) {
                                Ok(GraphCommand::Inspect(ids)) => {
                                    let as_of = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                                        .unwrap_or(0);
                                    match GraphMaintenanceApi::preview(character, &ids, as_of, GraphMaintenanceMode::Clear).await {
                                        Ok(preview) => {
                                            println!("Graph inspection for {character}: {} sources, {} existing derived relations. Read-only; no confirmation or mutation.", preview.entries().len(), preview.inspected_relations().len());
                                            for item in preview.inspected_relations() {
                                                println!("  source={} revision={} subject={} predicate={} object={}",
                                                    item.source.0, item.source_revision,
                                                    serde_json::to_string(&item.relation.subject).expect("string JSON"),
                                                    serde_json::to_string(&item.relation.predicate).expect("string JSON"),
                                                    serde_json::to_string(&item.relation.object).expect("string JSON"));
                                            }
                                        }
                                        Err(error) => eprintln!("Graph: {error}"),
                                    }
                                }
                                Ok(GraphCommand::Preview(mode, ids)) => {
                                    pending_graph = None;
                                    let as_of = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                                        .unwrap_or(0);
                                    match GraphMaintenanceApi::preview(character, &ids, as_of, mode).await {
                                        Ok(preview) => {
                                            let token = uuid::Uuid::new_v4();
                                            println!("Graph {:?}: {} выбранных источников, {} существующих связей. Scope: персонаж {character}. Источники:", mode, preview.entries().len(), preview.entries().iter().map(|item| item.existing_relations.len()).sum::<usize>());
                                            for item in preview.entries() {
                                                println!("  {}: {} связей", item.source.record.id.0, item.existing_relations.len());
                                            }
                                            if mode == GraphMaintenanceMode::Reindex && !GraphMaintenanceApi::reindex_available().unwrap_or(false) {
                                                println!("Reindex: производственный GraphExtractor не настроен; применение недоступно (BackendUnavailable).");
                                            } else {
                                                println!("Подтвердить одним действием: /knowledge graph apply {token}");
                                                pending_graph = Some((token, character, preview));
                                            }
                                        }
                                        Err(error) => eprintln!("Graph: {error}"),
                                    }
                                }
                                Ok(GraphCommand::Apply(token)) => {
                                    match take_scoped_preview(&mut pending_graph, character, token, "Graph") {
                                        Ok(plan) => {
                                            match GraphMaintenanceApi::apply_confirmed(
                                                character,
                                                plan,
                                                true,
                                                &tokio_util::sync::CancellationToken::new(),
                                            ).await {
                                                Ok(result) => println!("Graph: атомарно применено {} изменений. Канонические факты сохранены.", result.applied.len()),
                                                Err(error) => eprintln!("Graph: {error}; для повтора требуется новый preview."),
                                            }
                                        }
                                        Err(error) => eprintln!("Graph: {error}"),
                                    }
                                }
                                Err(error) => eprintln!("Graph: {error}"),
                            }
                        } else if slash_command(&line, "/knowledge") {
                            match parse_cache_maintenance(&line) {
                                Ok(CacheMaintenanceCommand::Preview) => {
                                    pending_cache_clear = None;
                                    match KnowledgeCacheApi::preview() {
                                        Ok(preview) => {
                                            let token = uuid::Uuid::new_v4();
                                            println!(
                                                "Кэш: {} записей; пространство {} / {} / {} ({}D); по персонажам: {:?}.\nДля подтверждённой очистки: /knowledge cache clear {token}",
                                                preview.total_entries,
                                                preview.space.backend,
                                                preview.space.model,
                                                preview.space.protocol,
                                                preview.space.dimensions,
                                                preview.entries_by_character
                                            );
                                            pending_cache_clear = Some((token, preview));
                                        }
                                        Err(error) => eprintln!("Knowledge: {error}"),
                                    }
                                }
                                Ok(CacheMaintenanceCommand::Clear(token)) => {
                                    match take_cache_clear_preview(&mut pending_cache_clear, token) {
                                        Ok(preview) => match KnowledgeCacheApi::clear_confirmed(&preview, true) {
                                            Ok(cleared) => println!("Очищено {cleared} производных embedding-записей. Timeline и память не изменены."),
                                            Err(error) => eprintln!("Knowledge: {error}; запросите новый preview."),
                                        },
                                        Err(error) => eprintln!("{error}"),
                                    }
                                }
                                Err(error) => eprintln!("{error}"),
                            }
                        } else if line == "/dataset status" {
                            println!("{}", dataset_status(dataset));
                        } else if let Some(value) = line.strip_prefix("/diagnostics") {
                            if let Err(error) = diagnostics_command(character, value.trim()) {
                                eprintln!("{error}");
                            }
                        } else if line == "/game" || line.starts_with("/game ") {
                            let value = line.strip_prefix("/game").unwrap_or("");
                            if let Some(id) = game_command(character, value.trim()).await {
                                pending.insert(id);
                            }
                        } else if line.starts_with("/reminder") {
                            match reminder_command(character, line.trim()).await {
                                Ok(text) => println!("{text}"),
                                Err(error) => eprintln!("{error}"),
                            }
                        } else if line.starts_with('/') {
                            eprintln!("Неизвестная управляющая команда.");
                        } else {
                            match presenter.reply(character, line) {
                                Ok(handle) => { println!("Accepted: {}", handle.id()); pending.insert(handle.id()); }
                                Err(error) => eprintln!("{error}"),
                            }
                        }
                    }
                }
            }
        }
    }
}

fn slash_command(line: &str, command: &str) -> bool {
    line.strip_prefix(command)
        .is_some_and(|tail| tail.is_empty() || tail.chars().next().is_some_and(char::is_whitespace))
}

#[derive(Debug, PartialEq, Eq)]
enum MemoryTransferCommand {
    Export(String),
    Preview(String),
    Apply(uuid::Uuid),
}

fn parse_memory_transfer(line: &str) -> Result<MemoryTransferCommand, &'static str> {
    const USAGE: &str = "Используй /knowledge memory export PATH | preview PATH | apply TOKEN";
    let rest = line.strip_prefix("/knowledge memory ").ok_or(USAGE)?;
    let (operation, value) = rest.split_once(char::is_whitespace).ok_or(USAGE)?;
    let value = unquote(value.trim());
    if value.is_empty() {
        return Err(USAGE);
    }
    match operation {
        "export" => Ok(MemoryTransferCommand::Export(value.to_owned())),
        "preview" => Ok(MemoryTransferCommand::Preview(value.to_owned())),
        "apply" if !value.chars().any(char::is_whitespace) => uuid::Uuid::parse_str(value)
            .map(MemoryTransferCommand::Apply)
            .map_err(|_| USAGE),
        _ => Err(USAGE),
    }
}

fn parse_variable_transfer(line: &str) -> Result<MemoryTransferCommand, &'static str> {
    const USAGE: &str = "Используй /knowledge variables export PATH | preview PATH | apply TOKEN";
    let rest = line.strip_prefix("/knowledge variables ").ok_or(USAGE)?;
    let (operation, value) = rest.split_once(char::is_whitespace).ok_or(USAGE)?;
    let value = unquote(value.trim());
    if value.is_empty() {
        return Err(USAGE);
    }
    match operation {
        "export" => Ok(MemoryTransferCommand::Export(value.to_owned())),
        "preview" => Ok(MemoryTransferCommand::Preview(value.to_owned())),
        "apply" if !value.chars().any(char::is_whitespace) => uuid::Uuid::parse_str(value)
            .map(MemoryTransferCommand::Apply)
            .map_err(|_| USAGE),
        _ => Err(USAGE),
    }
}

fn parse_portable_transfer(line: &str) -> Result<MemoryTransferCommand, &'static str> {
    const USAGE: &str = "Используй /knowledge portable export PATH | preview PATH | apply TOKEN";
    let rest = line.strip_prefix("/knowledge portable ").ok_or(USAGE)?;
    let (operation, value) = rest.split_once(char::is_whitespace).ok_or(USAGE)?;
    let value = unquote(value.trim());
    if value.is_empty() {
        return Err(USAGE);
    }
    match operation {
        "export" => Ok(MemoryTransferCommand::Export(value.to_owned())),
        "preview" => Ok(MemoryTransferCommand::Preview(value.to_owned())),
        "apply" if !value.chars().any(char::is_whitespace) => uuid::Uuid::parse_str(value)
            .map(MemoryTransferCommand::Apply)
            .map_err(|_| USAGE),
        _ => Err(USAGE),
    }
}

fn read_scoped_archive(path: &str, limit: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((limit as u64) + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid("archive exceeds bounded byte limit"));
    }
    Ok(bytes)
}

#[derive(Debug)]
enum ExportPublication {
    Durable,
    PublishedDurabilityUnconfirmed(String),
}

fn write_no_clobber_export(path: &str, bytes: &[u8]) -> io::Result<ExportPublication> {
    write_no_clobber_export_with_preflight(std::path::Path::new(path), bytes, || Ok(()))
}

/// Publishes only fully written and synced bytes, never replacing an existing path.
/// Hard-link publication is intentionally required; unsupported filesystems fail closed.
fn write_no_clobber_export_with_preflight(
    path: &std::path::Path,
    bytes: &[u8],
    before_publish: impl FnOnce() -> io::Result<()>,
) -> io::Result<ExportPublication> {
    if bytes.len() > MAX_PORTABLE_ARCHIVE_BYTES {
        return Err(invalid("archive export exceeds bounded byte limit"));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let _ = path
        .file_name()
        .ok_or_else(|| invalid("memory export requires a filename"))?;
    let mut pending = None;
    for _ in 0..8 {
        let temp = parent.join(format!(
            ".neuromita-memory-{}.pending",
            uuid::Uuid::new_v4()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(file) => {
                pending = Some((temp, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let (temp, mut file) = pending.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot allocate private export staging file",
        )
    })?;
    let prepublish = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .and_then(|_| before_publish());
    drop(file);
    let publication = prepublish.and_then(|_| std::fs::hard_link(&temp, path));
    if let Err(error) = publication {
        return match std::fs::remove_file(&temp) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(io::Error::other(format!(
                "export not published ({error}); private staging cleanup failed at {}: {cleanup}",
                temp.display()
            ))),
        };
    }
    let mut warnings = Vec::new();
    if let Err(error) = std::fs::remove_file(&temp) {
        warnings.push(format!(
            "private staging cleanup failed at {}: {error}",
            temp.display()
        ));
    }
    if let Err(error) = std::fs::File::open(parent).and_then(|directory| directory.sync_all()) {
        warnings.push(format!("directory sync unavailable: {error}"));
    }
    if warnings.is_empty() {
        Ok(ExportPublication::Durable)
    } else {
        Ok(ExportPublication::PublishedDurabilityUnconfirmed(
            warnings.join("; "),
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum GraphCommand {
    Preview(GraphMaintenanceMode, Vec<MemoryId>),
    Inspect(Vec<MemoryId>),
    Apply(uuid::Uuid),
}

fn parse_graph_maintenance(line: &str) -> Result<GraphCommand, &'static str> {
    const USAGE: &str = "Используй /knowledge graph inspect MEMORY_UUID... | preview clear|reindex MEMORY_UUID... | apply TOKEN";
    let parts = line.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["/knowledge", "graph", "apply", token] => uuid::Uuid::parse_str(token)
            .map(GraphCommand::Apply)
            .map_err(|_| USAGE),
        ["/knowledge", "graph", "inspect", ids @ ..] if !ids.is_empty() && ids.len() <= 256 => {
            let parsed = parse_graph_ids(ids, USAGE)?;
            Ok(GraphCommand::Inspect(parsed))
        }
        ["/knowledge", "graph", "preview", mode, ids @ ..]
            if !ids.is_empty() && ids.len() <= 256 =>
        {
            let mode = match *mode {
                "clear" => GraphMaintenanceMode::Clear,
                "reindex" => GraphMaintenanceMode::Reindex,
                _ => return Err(USAGE),
            };
            Ok(GraphCommand::Preview(mode, parse_graph_ids(ids, USAGE)?))
        }
        _ => Err(USAGE),
    }
}

fn parse_graph_ids(ids: &[&str], usage: &'static str) -> Result<Vec<MemoryId>, &'static str> {
    let parsed = ids
        .iter()
        .map(|id| uuid::Uuid::parse_str(id).map(MemoryId).map_err(|_| usage))
        .collect::<Result<Vec<_>, _>>()?;
    if parsed
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != parsed.len()
    {
        return Err(usage);
    }
    Ok(parsed)
}

fn take_scoped_preview<T>(
    pending: &mut Option<(uuid::Uuid, CharacterId, T)>,
    character: CharacterId,
    token: uuid::Uuid,
    kind: &'static str,
) -> Result<T, String> {
    match pending.as_ref() {
        Some((expected, owner, _)) if *expected == token && *owner == character => {
            Ok(pending.take().expect("matched confirmed preview").2)
        }
        Some((_, owner, _)) if *owner != character => {
            Err(format!("{kind} preview принадлежит другому персонажу"))
        }
        Some(_) => Err(format!("Неверный токен {kind}; данные не изменены")),
        None => Err(format!("Нет ожидающего {kind} preview")),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CacheMaintenanceCommand {
    Preview,
    Clear(uuid::Uuid),
}

fn parse_cache_maintenance(line: &str) -> Result<CacheMaintenanceCommand, &'static str> {
    const USAGE: &str = "Используй /knowledge cache preview, затем /knowledge cache clear TOKEN";
    match line.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["/knowledge", "cache", "preview"] => Ok(CacheMaintenanceCommand::Preview),
        ["/knowledge", "cache", "clear", token] => uuid::Uuid::parse_str(token)
            .map(CacheMaintenanceCommand::Clear)
            .map_err(|_| USAGE),
        _ => Err(USAGE),
    }
}

fn take_cache_clear_preview(
    pending: &mut Option<(uuid::Uuid, EmbeddingCachePreview)>,
    token: uuid::Uuid,
) -> Result<EmbeddingCachePreview, &'static str> {
    let Some((expected, _)) = pending else {
        return Err("Нет ожидающего подтверждения; выполните /knowledge cache preview");
    };
    if *expected != token {
        return Err("Неверный токен подтверждения; кэш не изменён");
    }
    Ok(pending.take().expect("matched pending preview").1)
}

#[derive(Debug, PartialEq, Eq)]
enum HistoryCommand<'a> {
    Keyword(&'a str),
    Fts(&'a str),
    Vector(&'a str),
    Fusion(&'a str),
}

fn parse_history_generation_mode(
    line: &str,
) -> Result<Option<HistoryGenerationMode>, &'static str> {
    const USAGE: &str = "Используй /rag generation [off|keyword|fts|vector|fusion|window N]";
    match line.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["/rag", "generation"] => Ok(None),
        ["/rag", "generation", "off"] => Ok(Some(HistoryGenerationMode::Off)),
        ["/rag", "generation", "keyword"] => Ok(Some(HistoryGenerationMode::Keyword)),
        ["/rag", "generation", "fts"] => Ok(Some(HistoryGenerationMode::Fts)),
        ["/rag", "generation", "vector"] => Ok(Some(HistoryGenerationMode::Vector)),
        ["/rag", "generation", "fusion"] => Ok(Some(HistoryGenerationMode::Fusion)),
        _ => Err(USAGE),
    }
}

fn parse_history_generation_window(line: &str) -> Result<Option<u8>, &'static str> {
    const USAGE: &str = "Используй /rag generation window N (1..64)";
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.get(2) != Some(&"window") {
        return Ok(None);
    }
    let ["/rag", "generation", "window", count] = parts.as_slice() else {
        return Err(USAGE);
    };
    let count = count.parse::<u8>().map_err(|_| USAGE)?;
    if !(1..=64).contains(&count) {
        return Err(USAGE);
    }
    Ok(Some(count))
}

fn history_generation_command(line: &str) -> Result<String, String> {
    let requested_window = parse_history_generation_window(line).map_err(str::to_owned)?;
    let requested = if requested_window.is_some() {
        None
    } else {
        parse_history_generation_mode(line).map_err(str::to_owned)?
    };
    let current =
        SettingsApi::snapshot::<HistoryGenerationSettings>().map_err(|error| error.to_string())?;
    let value = if let Some(count) = requested_window {
        let mut next = current.value;
        next.active_messages = count;
        SettingsApi::replace(current.revision, next)
            .map_err(|error| error.to_string())?
            .value
    } else {
        match requested {
            None => current.value,
            Some(mode) => {
                let mut next = current.value;
                next.mode = mode;
                SettingsApi::replace(current.revision, next)
                    .map_err(|error| error.to_string())?
                    .value
            }
        }
    };
    Ok(format!(
        "Генерация: History RAG {:?}, {} активных сообщений, {} результатов, {} символов на запись; изменение влияет только на новые запросы.",
        value.mode, value.active_messages, value.max_results, value.max_content_chars
    ))
}

fn parse_history_search(line: &str) -> Result<HistoryCommand<'_>, &'static str> {
    const USAGE: &str = "Используй /rag history QUERY, /rag history-fts QUERY, /rag history-vector QUERY или /rag history-fusion QUERY";
    let (selection, query) = if let Some(query) = line.strip_prefix("/rag history ") {
        (0, query)
    } else if let Some(query) = line.strip_prefix("/rag history-fts ") {
        (3, query)
    } else if let Some(query) = line.strip_prefix("/rag history-vector ") {
        (1, query)
    } else if let Some(query) = line.strip_prefix("/rag history-fusion ") {
        (2, query)
    } else {
        return Err(USAGE);
    };
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 2048 {
        return Err(USAGE);
    }
    Ok(match selection {
        0 => HistoryCommand::Keyword(query),
        1 => HistoryCommand::Vector(query),
        2 => HistoryCommand::Fusion(query),
        _ => HistoryCommand::Fts(query),
    })
}

fn parse_forgotten_recall(line: &str) -> Result<(RecallSelector, String), &'static str> {
    const USAGE: &str =
        "Используй /recall forgotten id UUID | QUERY или /recall forgotten search | QUERY";
    let rest = line.strip_prefix("/recall forgotten ").ok_or(USAGE)?;
    let (selection, query) = rest.split_once('|').ok_or(USAGE)?;
    let selection = selection.trim();
    let query = query.trim();
    if query.is_empty() {
        return Err(USAGE);
    }
    let selector = if let Some(raw_id) = selection.strip_prefix("id ") {
        let id = uuid::Uuid::parse_str(raw_id.trim()).map_err(|_| USAGE)?;
        RecallSelector::Ids(vec![MemoryId(id)])
    } else if selection == "search" {
        RecallSelector::Text {
            query: query.to_owned(),
            limit: 12,
        }
    } else {
        return Err(USAGE);
    };
    Ok((selector, query.to_owned()))
}

#[cfg(test)]
mod forgotten_recall_command_tests {
    use super::*;

    #[test]
    fn explicit_recall_parses_scoped_search_or_exact_uuid_only() {
        let (selector, query) =
            parse_forgotten_recall("/recall forgotten search | green tea").unwrap();
        assert_eq!(query, "green tea");
        assert_eq!(
            selector,
            RecallSelector::Text {
                query: query.clone(),
                limit: 12
            }
        );

        let id = uuid::Uuid::new_v4();
        let (selector, query) =
            parse_forgotten_recall(&format!("/recall forgotten id {id} | player")).unwrap();
        assert_eq!(selector, RecallSelector::Ids(vec![MemoryId(id)]));
        assert_eq!(query, "player");
    }

    #[test]
    fn malformed_slash_prefixes_are_control_commands_not_chat_messages() {
        for command in [
            "/recall",
            "/recall forgotten",
            "/recall\tforgotten search | tea",
        ] {
            assert!(slash_command(command, "/recall"));
            assert!(parse_forgotten_recall(command).is_err());
        }
        assert!(!slash_command("/recaller", "/recall"));
        assert!(slash_command("/rag\thistory tea", "/rag"));
        assert!(parse_history_search("/rag\thistory tea").is_err());
        assert_eq!(
            parse_history_search("/rag history green tea"),
            Ok(HistoryCommand::Keyword("green tea"))
        );
        assert_eq!(
            parse_history_search("/rag history-fts green tea"),
            Ok(HistoryCommand::Fts("green tea"))
        );
        assert_eq!(
            parse_history_search("/rag history-vector green tea"),
            Ok(HistoryCommand::Vector("green tea"))
        );
        assert_eq!(
            parse_history_search("/rag history-fusion green tea"),
            Ok(HistoryCommand::Fusion("green tea"))
        );
        assert!(parse_history_search("/rag history-vector").is_err());
        assert_eq!(
            parse_history_generation_mode("/rag generation").unwrap(),
            None
        );
        assert_eq!(
            parse_history_generation_mode("/rag generation off"),
            Ok(Some(HistoryGenerationMode::Off))
        );
        assert_eq!(
            parse_history_generation_mode("/rag generation fusion"),
            Ok(Some(HistoryGenerationMode::Fusion))
        );
        assert!(parse_history_generation_mode("/rag generation not-a-mode").is_err());
        assert!(parse_history_generation_mode("/rag generation fusion extra").is_err());
        assert_eq!(
            parse_history_generation_window("/rag generation window 3"),
            Ok(Some(3))
        );
        assert_eq!(parse_history_generation_window("/rag generation"), Ok(None));
        for invalid in [
            "/rag generation window",
            "/rag generation window 0",
            "/rag generation window 65",
            "/rag generation window 2 extra",
            "/rag generation window x",
        ] {
            assert!(parse_history_generation_window(invalid).is_err());
        }
        assert_eq!(
            parse_history_generation_mode("/rag\tgeneration fusion"),
            Ok(Some(HistoryGenerationMode::Fusion))
        );
    }

    #[test]
    fn portable_transfer_cli_requires_explicit_scoped_confirmation() {
        let token = uuid::Uuid::new_v4();
        assert_eq!(
            parse_portable_transfer("/knowledge portable export bundle.json"),
            Ok(MemoryTransferCommand::Export("bundle.json".into()))
        );
        assert_eq!(
            parse_portable_transfer("/knowledge portable preview \"scope archive.json\""),
            Ok(MemoryTransferCommand::Preview("scope archive.json".into()))
        );
        assert_eq!(
            parse_portable_transfer(&format!("/knowledge portable apply {token}")),
            Ok(MemoryTransferCommand::Apply(token))
        );
        for bad in [
            "/knowledge portable",
            "/knowledge portable preview",
            "/knowledge portable apply yes",
            "/knowledge portable apply 00000000-0000-0000-0000-000000000000 extra",
            "/knowledge portable purge all",
        ] {
            assert!(parse_portable_transfer(bad).is_err());
            assert!(slash_command(bad, "/knowledge portable"));
        }
    }

    #[test]
    fn variable_transfer_cli_requires_explicit_scoped_token() {
        let token = uuid::Uuid::new_v4();
        assert_eq!(
            parse_variable_transfer("/knowledge variables export user.json"),
            Ok(MemoryTransferCommand::Export("user.json".into()))
        );
        assert_eq!(
            parse_variable_transfer("/knowledge variables preview \"path with spaces.json\""),
            Ok(MemoryTransferCommand::Preview(
                "path with spaces.json".into()
            ))
        );
        assert_eq!(
            parse_variable_transfer(&format!("/knowledge variables apply {token}")),
            Ok(MemoryTransferCommand::Apply(token))
        );
        for bad in [
            "/knowledge variables",
            "/knowledge variables export",
            "/knowledge variables preview",
            "/knowledge variables apply",
            "/knowledge variables apply yes",
            "/knowledge variables purge all",
            "/knowledge variables apply 00000000-0000-0000-0000-000000000000 extra",
        ] {
            assert!(parse_variable_transfer(bad).is_err());
            assert!(slash_command(bad, "/knowledge variables"));
        }
        let mut pending = Some((token, CharacterId::Crazy, "variables"));
        assert!(take_scoped_preview(&mut pending, CharacterId::Kind, token, "Variables").is_err());
        assert!(pending.is_some());
        assert!(
            take_scoped_preview(
                &mut pending,
                CharacterId::Crazy,
                uuid::Uuid::new_v4(),
                "Variables"
            )
            .is_err()
        );
        assert_eq!(
            take_scoped_preview(&mut pending, CharacterId::Crazy, token, "Variables"),
            Ok("variables")
        );
        assert!(pending.is_none());
    }

    #[test]
    fn memory_transfer_requires_exact_local_confirm_token_and_no_overwrite_file() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            parse_memory_transfer("/knowledge memory export backup.json"),
            Ok(MemoryTransferCommand::Export("backup.json".into()))
        );
        assert_eq!(
            parse_memory_transfer("/knowledge memory preview \"with spaces.json\""),
            Ok(MemoryTransferCommand::Preview("with spaces.json".into()))
        );
        assert_eq!(
            parse_memory_transfer(&format!("/knowledge memory apply {id}")),
            Ok(MemoryTransferCommand::Apply(id))
        );
        for command in [
            "/knowledge memory",
            "/knowledge memory export",
            "/knowledge memory preview",
            "/knowledge memory apply",
            "/knowledge memory apply yes",
            "/knowledge memory apply 00000000-0000-0000-0000-000000000000 extra",
            "/knowledge memory purge all",
        ] {
            assert!(
                parse_memory_transfer(command).is_err(),
                "accepted {command}"
            );
            assert!(slash_command(command, "/knowledge memory"));
        }
        let path = std::env::temp_dir().join(format!("memory-export-test-{id}.json"));
        let path = path.to_str().unwrap();
        write_no_clobber_export(path, b"validated export").unwrap();
        assert!(write_no_clobber_export(path, b"must not replace").is_err());
        assert_eq!(
            read_scoped_archive(path, MAX_ARCHIVE_BYTES).unwrap(),
            b"validated export"
        );
        std::fs::remove_file(path).unwrap();
        let mut pending = Some((id, CharacterId::Crazy, "preview"));
        assert!(take_scoped_preview(&mut pending, CharacterId::Kind, id, "Memory").is_err());
        assert!(pending.is_some());
        assert!(
            take_scoped_preview(
                &mut pending,
                CharacterId::Crazy,
                uuid::Uuid::new_v4(),
                "Memory"
            )
            .is_err()
        );
        assert_eq!(
            take_scoped_preview(&mut pending, CharacterId::Crazy, id, "Memory").unwrap(),
            "preview"
        );
        assert!(take_scoped_preview(&mut pending, CharacterId::Crazy, id, "Memory").is_err());
    }

    #[test]
    fn memory_export_is_atomic_no_clobber_and_cleans_failed_staging() {
        let root =
            std::env::temp_dir().join(format!("memory-export-atomic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("memory.json");
        let failed = write_no_clobber_export_with_preflight(&dest, b"private test archive", || {
            Err(io::Error::other("injected failure before publication"))
        });
        assert!(failed.is_err());
        assert!(!dest.exists());
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
        let done = write_no_clobber_export(dest.to_str().unwrap(), b"complete archive").unwrap();
        #[cfg(unix)]
        assert!(matches!(done, ExportPublication::Durable));
        #[cfg(not(unix))]
        let _ = done;
        assert_eq!(std::fs::read(&dest).unwrap(), b"complete archive");
        assert!(write_no_clobber_export(dest.to_str().unwrap(), b"dangerous replacement").is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"complete archive");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        #[cfg(unix)]
        {
            let target = root.join("target.txt");
            std::fs::write(&target, b"original target").unwrap();
            let link = root.join("symlink.json");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(write_no_clobber_export(link.to_str().unwrap(), b"changed").is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"original target");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_export_publish_has_one_writer_and_no_partial_destination() {
        let root =
            std::env::temp_dir().join(format!("memory-export-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let destination = root.join("export.json");
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first = std::thread::spawn({
            let destination = destination.clone();
            let ready = std::sync::Arc::clone(&ready);
            move || {
                ready.wait();
                write_no_clobber_export(destination.to_str().unwrap(), b"archive A")
            }
        });
        let second = std::thread::spawn({
            let destination = destination.clone();
            move || {
                ready.wait();
                write_no_clobber_export(destination.to_str().unwrap(), b"archive B")
            }
        });
        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| result
                    .as_ref()
                    .is_err_and(|err| err.kind() == io::ErrorKind::AlreadyExists))
                .count(),
            1
        );
        let persisted = std::fs::read(destination).unwrap();
        assert!(persisted == b"archive A" || persisted == b"archive B");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn graph_command_requires_explicit_unique_ids_and_token() {
        let id = MemoryId::new();
        let token = uuid::Uuid::new_v4();
        assert_eq!(
            parse_graph_maintenance(&format!("/knowledge graph preview clear {}", id.0)),
            Ok(GraphCommand::Preview(GraphMaintenanceMode::Clear, vec![id]))
        );
        assert_eq!(
            parse_graph_maintenance(&format!("/knowledge graph inspect {}", id.0)),
            Ok(GraphCommand::Inspect(vec![id]))
        );
        assert_eq!(
            parse_graph_maintenance(&format!("/knowledge graph preview reindex {}", id.0)),
            Ok(GraphCommand::Preview(
                GraphMaintenanceMode::Reindex,
                vec![id]
            ))
        );
        assert_eq!(
            parse_graph_maintenance(&format!("/knowledge graph apply {token}")),
            Ok(GraphCommand::Apply(token))
        );
        let mut pending = Some((token, CharacterId::Crazy, "graph preview"));
        assert!(take_scoped_preview(&mut pending, CharacterId::Kind, token, "Graph").is_err());
        assert!(pending.is_some());
        assert!(
            take_scoped_preview(
                &mut pending,
                CharacterId::Crazy,
                uuid::Uuid::new_v4(),
                "Graph"
            )
            .is_err()
        );
        assert!(pending.is_some());
        assert_eq!(
            take_scoped_preview(&mut pending, CharacterId::Crazy, token, "Graph").unwrap(),
            "graph preview"
        );
        assert!(take_scoped_preview(&mut pending, CharacterId::Crazy, token, "Graph").is_err());
        for invalid in [
            "/knowledge graph preview",
            "/knowledge graph preview clear",
            "/knowledge graph preview clear not-a-uuid",
            "/knowledge graph inspect",
            "/knowledge graph inspect not-a-uuid",
            "/knowledge graph apply yes",
            "/knowledge graph apply",
            "/knowledge graph preview purge 00000000-0000-0000-0000-000000000001",
            &format!("/knowledge graph preview clear {} {}", id.0, id.0),
        ] {
            assert!(parse_graph_maintenance(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn cache_clear_requires_opaque_preview_token_and_exact_explicit_command() {
        assert_eq!(
            parse_cache_maintenance("/knowledge cache preview"),
            Ok(CacheMaintenanceCommand::Preview)
        );
        let token = uuid::Uuid::new_v4();
        assert_eq!(
            parse_cache_maintenance(&format!("/knowledge cache clear {token}")),
            Ok(CacheMaintenanceCommand::Clear(token))
        );
        for malformed in [
            "/knowledge",
            "/knowledge cache",
            "/knowledge cache clear",
            "/knowledge cache clear yes",
            "/knowledge cache preview extra",
            "/knowledge cache clear 00000000-0000-0000-0000-000000000001 extra",
        ] {
            assert!(slash_command(malformed, "/knowledge"));
            assert!(parse_cache_maintenance(malformed).is_err());
        }
        struct FakeEmbedding(neuromita::knowledge::embedding::EmbeddingSpace);
        impl neuromita::knowledge::embedding::EmbeddingProvider for FakeEmbedding {
            fn space(&self) -> &neuromita::knowledge::embedding::EmbeddingSpace {
                &self.0
            }
            fn embed(
                &self,
                _: neuromita::knowledge::embedding::EmbeddingPurpose,
                _: String,
            ) -> neuromita::knowledge::embedding::EmbeddingFuture<
                '_,
                neuromita::knowledge::embedding::Embedding,
            > {
                Box::pin(async { unreachable!("preview must not use inference") })
            }
        }
        let retriever = neuromita::knowledge::semantic_memory::SemanticMemoryRetriever::new(
            Arc::new(FakeEmbedding(
                neuromita::knowledge::embedding::EmbeddingSpace::new("test", "model", "v1", 2)
                    .unwrap(),
            )),
            16,
        )
        .unwrap();
        let preview = retriever.preview_cache_clear();
        let mut pending = Some((token, preview));
        assert!(take_cache_clear_preview(&mut pending, uuid::Uuid::new_v4()).is_err());
        assert!(pending.is_some());
        let taken = take_cache_clear_preview(&mut pending, token).unwrap();
        assert!(pending.is_none());
        assert_eq!(taken.total_entries, 0);
        assert!(take_cache_clear_preview(&mut pending, token).is_err());
    }

    #[test]
    fn malformed_or_implicit_recall_never_becomes_chat_text() {
        for invalid in [
            "/recall",
            "/recall forgotten",
            "/recall forgotten search",
            "/recall forgotten search | ",
            "/recall forgotten id not-a-uuid | tea",
            "/recall forgotten id 00000000-0000-0000-0000-000000000001",
            "/recall forgotten all | tea",
        ] {
            assert!(
                parse_forgotten_recall(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }
}

fn diagnostics_command(character: CharacterId, arguments: &str) -> io::Result<()> {
    let access =
        ChatApi::diagnostics_for(DiagnosticScope::owner(character, RequestSource::Presenter))
            .map_err(|error| io::Error::other(error.to_string()))?;
    let mut parts = Arguments::new(arguments);
    match parts.next() {
        None | Some("help") => {
            println!(
                "/diagnostics latest [section]              exact latest trace in this scope\n\
                 /diagnostics list [limit]                  recent trace summaries\n\
                 /diagnostics errors [limit]                bounded error history\n\
                 /diagnostics ID [section]                  exact trace; no fallback\n\
                 /diagnostics view ID [section]             print JSON for copying\n\
                 /diagnostics export ID [section] PATH      write section JSON to PATH\n\
                 sections: all|ingress|prompt|request|response|usage|error|lifecycle"
            );
            Ok(())
        }
        Some("latest") => {
            let section = section(parts.next())?;
            let record = access
                .latest()
                .map_err(|error| io::Error::other(error.to_string()))?;
            println!(
                "{}",
                record
                    .export(section)
                    .map_err(|error| io::Error::other(error.to_string()))?
            );
            Ok(())
        }
        Some("list") => {
            let limit = limit(parts.next())?;
            let rows = access
                .recent(limit)
                .map_err(|error| io::Error::other(error.to_string()))?;
            for row in rows {
                println!(
                    "{} {} {:?} {:?} {}ms",
                    row.request_id,
                    row.character,
                    row.outcome.unwrap_or("incomplete"),
                    row.source,
                    row.elapsed_ms
                );
            }
            Ok(())
        }
        Some("errors") => {
            let limit = limit(parts.next())?;
            for error in access
                .errors(limit)
                .map_err(|error| io::Error::other(error.to_string()))?
            {
                println!(
                    "{} {} {:?} {:?} {}{}",
                    error.request_id,
                    error.character,
                    error.stage,
                    error.source,
                    error.message.as_str(),
                    if error.malformed { " (malformed)" } else { "" }
                );
            }
            Ok(())
        }
        Some("view") => print_exact(&access, parts.next(), parts.next()),
        Some("export") => {
            let id = parse_id(parts.next())?;
            let section = section(parts.next())?;
            let path = unquote(parts.rest());
            if path.is_empty() {
                return Err(invalid("export requires a path"));
            }
            let record = access
                .request(id)
                .map_err(|error| io::Error::other(error.to_string()))?;
            let text = record
                .export(section)
                .map_err(|error| io::Error::other(error.to_string()))?;
            write_export(path, &text)?;
            println!(
                "Exported {section} for {id} to {path} ({} bytes)",
                text.len()
            );
            Ok(())
        }
        Some(id) => {
            let id = parse_id(Some(id))?;
            let section = section(parts.next())?;
            let record = access
                .request(id)
                .map_err(|error| io::Error::other(error.to_string()))?;
            println!(
                "{}",
                record
                    .copy_text(section)
                    .map_err(|error| io::Error::other(error.to_string()))?
            );
            Ok(())
        }
    }
}

fn print_exact(
    access: &neuromita::request::DiagnosticsAccess,
    id: Option<&str>,
    section_value: Option<&str>,
) -> io::Result<()> {
    let id = parse_id(id)?;
    let section = section(section_value)?;
    let record = access
        .request(id)
        .map_err(|error| io::Error::other(error.to_string()))?;
    println!(
        "{}",
        record
            .copy_text(section)
            .map_err(|error| io::Error::other(error.to_string()))?
    );
    Ok(())
}

fn parse_id(value: Option<&str>) -> io::Result<RequestId> {
    value
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(RequestId)
        .ok_or_else(|| invalid("a request UUID is required"))
}

fn section(value: Option<&str>) -> io::Result<DiagnosticSection> {
    match value {
        None => Ok(DiagnosticSection::All),
        Some(value) => DiagnosticSection::parse(value).ok_or_else(|| {
            invalid("unknown diagnostic section; use all|ingress|prompt|request|response|usage|error|lifecycle")
        }),
    }
}

fn limit(value: Option<&str>) -> io::Result<usize> {
    match value {
        None => Ok(16),
        Some(value) => value
            .parse::<usize>()
            .map(|limit| limit.clamp(1, 64))
            .map_err(|_| invalid("limit must be a positive integer")),
    }
}

fn write_export(path: &str, text: &str) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

struct Arguments<'a> {
    value: &'a str,
    offset: usize,
}

impl<'a> Arguments<'a> {
    fn new(value: &'a str) -> Self {
        Self { value, offset: 0 }
    }

    fn next(&mut self) -> Option<&'a str> {
        let rest = self.value[self.offset..].trim_start();
        if rest.is_empty() {
            return None;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        self.offset = self.value.len() - rest.len() + end;
        Some(&rest[..end])
    }

    fn rest(&mut self) -> &'a str {
        let rest = self.value[self.offset..].trim_start();
        self.offset = self.value.len();
        rest
    }
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_speech_is_explicit_and_cannot_be_duplicated() {
        let required = [
            "--model",
            "gemini-2.5-flash",
            "--character",
            "Crazy",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            "/tmp/neuromita-opt-in-test",
        ];
        let parse = |extra: &[&str]| {
            Options::parse_args(required.iter().chain(extra).map(|arg| (*arg).to_owned()))
        };
        assert!(!parse(&[]).unwrap().unwrap().installed_speech);
        assert!(
            parse(&["--installed-speech"])
                .unwrap()
                .unwrap()
                .installed_speech
        );
        assert!(parse(&["--installed-speech", "--installed-speech"]).is_err());
        assert!(parse(&["--installed-speech", "--unknown"]).is_err());
    }

    #[test]
    fn export_never_overwrites_and_paths_may_contain_spaces() {
        let directory =
            std::env::temp_dir().join(format!("neuromita-terminal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let existing = directory.join("existing.json");
        std::fs::write(&existing, b"keep").unwrap();
        assert!(write_export(existing.to_str().unwrap(), "replace").is_err());
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep");

        let spaced = directory.join("trace with space.json");
        write_export(spaced.to_str().unwrap(), "{}").unwrap();
        assert_eq!(std::fs::read(&spaced).unwrap(), b"{}");

        let mut arguments = Arguments::new("request usage \"C:/tmp/trace with space.json\"");
        assert_eq!(arguments.next(), Some("request"));
        assert_eq!(arguments.next(), Some("usage"));
        assert_eq!(unquote(arguments.rest()), "C:/tmp/trace with space.json");
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod dataset_delivery_tests {
    use super::*;
    use neuromita::finetune::{
        FineTuneCollector, FineTuneSettings, Retention, SampleId, SampleOutcome,
    };
    #[tokio::test]
    async fn runtime_dataset_status_reports_late_writer_failure() {
        let root = std::env::temp_dir().join(format!("pr67-status-{}", uuid::Uuid::new_v4()));
        let collector = FineTuneCollector::from_settings(
            &FineTuneSettings::new(root.clone(), true, Retention::Unlimited).unwrap(),
        )
        .unwrap();
        std::fs::create_dir(root.join(".neuromita-finetune.lock")).unwrap();
        let request = RequestId(uuid::Uuid::new_v4());
        collector
            .enqueue_outcome(
                SampleId::for_request(request),
                request,
                None,
                SampleOutcome::Accepted,
            )
            .unwrap();
        collector
            .drain(std::time::Duration::from_secs(2), true)
            .await;
        let output = dataset_status(Some(&collector));
        assert!(output.contains("errors=1"), "{output}");
        assert!(output.contains("pending=0"), "{output}");
        assert!(output.contains(".neuromita-finetune.lock"), "{output}");
        drop(collector);
        std::fs::remove_dir_all(root).unwrap();
    }
}

fn print_board(label: &str, board: &[String]) {
    println!("{label}");
    println!(
        "  {}",
        ('A'..='J')
            .map(|letter| letter.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    for (index, row) in board.iter().enumerate() {
        println!(
            "{:<2}{}",
            index + 1,
            row.chars()
                .map(|cell| cell.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}

fn show_game(receipt: &GameReceipt) {
    match (&receipt.view, receipt.key) {
        (Some(view), Some(key)) => {
            println!(
                "Sea Battle: {} | {} | сессия {} rev {} | авто: {}",
                view.phase.code(),
                view.status_line_for(Side::Player),
                key.session,
                key.revision,
                receipt.auto_reactions
            );
            if !view.remaining.is_empty() {
                println!(
                    "Корабли к расстановке: {}",
                    view.remaining
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
            print_board("Твоё поле:", &view.own_board);
            print_board("Поле противника:", &view.opponent_board);
            if let Some(last) = view.last_shot {
                println!(
                    "Последний выстрел: {} по {} — {}.",
                    last.attacker.label(),
                    last.coord,
                    last.result.code()
                );
            }
            if let Some(winner) = view.winner {
                let winner = if winner == Side::Player {
                    "игрок"
                } else {
                    "Мита"
                };
                println!("Победитель: {winner}");
            }
        }
        _ => println!("Активной партии нет."),
    }
}

fn parse_coord(value: Option<&str>) -> Result<Coordinate, String> {
    value
        .and_then(Coordinate::parse)
        .ok_or_else(|| "координата должна быть в виде A1..J10".to_owned())
}

fn no_extra_args(parts: &mut std::str::SplitWhitespace, usage: &str) -> bool {
    if parts.next().is_some() {
        eprintln!("usage: {usage}");
        false
    } else {
        true
    }
}

async fn game_command(character: CharacterId, rest: &str) -> Option<RequestId> {
    let mut parts = rest.split_whitespace();
    let command = parts.next().unwrap_or("view");
    let result = match command {
        "start" => {
            let auto = match parts.next() {
                None | Some("auto") => true,
                Some("manual") => false,
                Some(_) => {
                    eprintln!("usage: /game start [auto|manual]");
                    return None;
                }
            };
            if !no_extra_args(&mut parts, "/game start [auto|manual]") {
                return None;
            }
            GameApi::start(character, auto).await
        }
        "place" => {
            let coord = match parse_coord(parts.next()) {
                Ok(coord) => coord,
                Err(message) => {
                    eprintln!("{message}");
                    return None;
                }
            };
            let length = match parts.next().and_then(|value| value.parse::<u8>().ok()) {
                Some(length @ 1..=4) => length,
                _ => {
                    eprintln!("длина корабля должна быть 1..4");
                    return None;
                }
            };
            let orientation = match Orientation::parse(parts.next().unwrap_or("H")) {
                Some(orientation) => orientation,
                None => {
                    eprintln!("ориентация должна быть H или V");
                    return None;
                }
            };
            if !no_extra_args(&mut parts, "/game place COORD LENGTH [H|V]") {
                return None;
            }
            GameApi::place_ship(character, coord, length, orientation).await
        }
        "random" => {
            if !no_extra_args(&mut parts, "/game random") {
                return None;
            }
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos() as u64)
                .unwrap_or(1);
            GameApi::place_fleet_randomly(character, seed).await
        }
        "fire" => {
            let coord = match parse_coord(parts.next()) {
                Ok(coord) => coord,
                Err(message) => {
                    eprintln!("{message}");
                    return None;
                }
            };
            if !no_extra_args(&mut parts, "/game fire COORD") {
                return None;
            }
            GameApi::fire(character, coord).await
        }
        "turn" => {
            if !no_extra_args(&mut parts, "/game turn") {
                return None;
            }
            GameApi::request_mita_turn(character).await
        }
        "auto" => {
            let enabled = match parts.next() {
                Some("on") => true,
                Some("off") => false,
                _ => {
                    eprintln!("usage: /game auto on|off");
                    return None;
                }
            };
            if !no_extra_args(&mut parts, "/game auto on|off") {
                return None;
            }
            GameApi::set_auto_reactions(character, enabled).await
        }
        "close" => {
            if !no_extra_args(&mut parts, "/game close") {
                return None;
            }
            GameApi::close(character).await
        }
        "view" => {
            if !no_extra_args(&mut parts, "/game view") {
                return None;
            }
            match GameApi::view(character).await {
                Ok(receipt) => {
                    show_game(&receipt);
                    return None;
                }
                Err(error) => {
                    eprintln!("Игра: {error}");
                    return None;
                }
            }
        }
        other => {
            eprintln!(
                "Неизвестная команда '{other}'. Используй start/place/random/fire/turn/auto/close/view."
            );
            return None;
        }
    };
    match result {
        Ok(outcome) => {
            show_game(&outcome.receipt);
            match outcome.reaction {
                None => None,
                Some(Ok(handle)) => Some(handle.id()),
                Some(Err(error)) => {
                    eprintln!("Реакция Миты не принята: {error}");
                    None
                }
            }
        }
        Err(error) => {
            eprintln!("Игра: {error}");
            None
        }
    }
}

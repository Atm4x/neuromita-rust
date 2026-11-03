use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::Arc,
};

use neuromita::{
    api::ChatApi,
    character::CharacterId,
    presentation::ChatPresenter,
    request::{
        DiagnosticScope, DiagnosticSection, RequestId, RequestOutcome, RequestSnapshot,
        RequestSource, RequestState,
    },
};
use tokio::sync::mpsc;

pub struct Options {
    pub model: String,
    character: CharacterId,
    message: Option<String>,
    pub listen: std::net::SocketAddr,
    pub data_dir: PathBuf,
    pub finetune_config: Option<PathBuf>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

impl Options {
    pub fn parse() -> io::Result<Option<Self>> {
        let mut args = std::env::args().skip(1);
        let mut values = HashMap::new();
        while let Some(argument) = args.next() {
            if argument == "--help" {
                println!(
                    "neuromita --model MODEL --character mita|kind-mita --listen HOST:PORT [--message TEXT] [--data-dir PATH] [--finetune-config PATH]\n\nRequired environment: GEMINI_API_KEY\nNetwork command: test followed by newline\nInteractive commands: /cancel REQUEST_UUID, /character mita|kind-mita, /diagnostics ..., /quit\n\n{}",
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
            .ok_or_else(|| invalid("unknown character; use mita or kind-mita"))?;
        let listen = values
            .remove("--listen")
            .ok_or_else(|| invalid("--listen is required"))?
            .parse()
            .map_err(|_| invalid("invalid listen address"))?;
        Ok(Some(Self {
            model,
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

pub async fn run(presenter: &ChatPresenter, options: Options) -> io::Result<()> {
    let mut character = options.character;
    let mut updates = presenter.observe();
    let mut printed: HashMap<RequestId, u64> = HashMap::new();
    let mut pending = HashSet::new();
    let mut failed = false;
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
        if eof && pending.is_empty() {
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
                                Some(id) => { character = id; println!("Character: {id}"); }
                                None => eprintln!("Unknown character"),
                            }
                        } else if let Some(value) = line.strip_prefix("/cancel ") {
                            match uuid::Uuid::parse_str(value) {
                                Ok(id) => match presenter.cancel(RequestId(id)) {
                                    Ok(result) => println!("Cancel: {result:?}"),
                                    Err(error) => eprintln!("{error}"),
                                }
                                Err(_) => eprintln!("Invalid request UUID"),
                            }
                        } else if let Some(value) = line.strip_prefix("/diagnostics") {
                            if let Err(error) = diagnostics_command(character, value.trim()) {
                                eprintln!("{error}");
                            }
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

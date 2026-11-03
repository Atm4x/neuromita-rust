use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead},
    path::PathBuf,
    sync::Arc,
};

use neuromita::{
    character::CharacterId,
    games::{
        GameApi, GameReceipt,
        seabattle::{Coordinate, Orientation, Side},
    },
    presentation::ChatPresenter,
    request::{RequestId, RequestOutcome, RequestSnapshot, RequestState},
};
use tokio::sync::mpsc;

pub struct Options {
    pub model: String,
    character: CharacterId,
    message: Option<String>,
    pub listen: std::net::SocketAddr,
    pub data_dir: PathBuf,
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
                    "neuromita --model MODEL --character mita|kind-mita --listen HOST:PORT [--message TEXT] [--data-dir PATH]\n\nRequired environment: GEMINI_API_KEY\nNetwork command: test followed by newline\nInteractive commands: /cancel REQUEST_UUID, /character mita|kind-mita, /game start|place|random|fire|turn|auto|close|view, /quit\n\n{}",
                    crate::settings_cli::HELP
                );
                return Ok(None);
            }
            if !matches!(
                argument.as_str(),
                "--model" | "--character" | "--message" | "--listen" | "--data-dir"
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
                        } else if line == "/game" || line.starts_with("/game ") {
                            let value = line.strip_prefix("/game").unwrap_or("");
                            if let Some(id) = game_command(character, value.trim()).await {
                                pending.insert(id);
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

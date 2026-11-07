//! Bounded UCI subprocess adapter. It proposes moves, never commits game state.
//! Each request owns a child process and cancels/kills it on timeout/drop.
use std::{path::PathBuf, process::Stdio, time::Duration};
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, Command},
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

const MAX_OUTPUT_BYTES: u64 = 64 * 1024;
const MAX_OUTPUT_LINES: usize = 256;
const REAP_GRACE: Duration = Duration::from_millis(200);

#[derive(Debug, Error)]
pub enum UciError {
    #[error("invalid UCI request: {0}")]
    Invalid(&'static str),
    #[error("failed to start the chess engine: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("UCI transport failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("UCI protocol violation: {0}")]
    Protocol(&'static str),
    #[error("UCI {0} timed out")]
    Timeout(&'static str),
    #[error("UCI operation cancelled")]
    Cancelled,
}

/// User-supplied executable and options. No automatic model downloads or
/// simulated Maia strength. Production Lc0 weights may be supplied as options.
#[derive(Clone, Debug)]
pub struct UciConfig {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub options: Vec<(String, String)>,
    pub environment: Vec<(String, String)>,
    pub startup_timeout: Duration,
    pub search_timeout: Duration,
    pub move_time: Duration,
}

impl UciConfig {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            options: Vec::new(),
            environment: Vec::new(),
            startup_timeout: Duration::from_secs(10),
            search_timeout: Duration::from_secs(30),
            move_time: Duration::from_secs(1),
        }
    }

    fn validate(&self) -> Result<(), UciError> {
        if self.executable.as_os_str().is_empty() {
            return Err(UciError::Invalid("engine executable is empty"));
        }
        if self.startup_timeout.is_zero() || self.search_timeout.is_zero() {
            return Err(UciError::Invalid("timeouts must be positive"));
        }
        if self.move_time.as_millis() == 0 || self.move_time.as_millis() > 60_000 {
            return Err(UciError::Invalid("move time must be 1..60000 ms"));
        }
        if self.args.iter().any(|s| has_control(s))
            || self.options.iter().any(|(name, value)| {
                name.trim().is_empty() || has_control(name) || has_control(value)
            })
            || self.environment.iter().any(|(key, value)| {
                key.is_empty() || key.contains('=') || has_control(key) || has_control(value)
            })
        {
            return Err(UciError::Invalid("invalid engine arguments or options"));
        }
        Ok(())
    }
}

/// Root position plus accepted history. The caller must supply a snapshot from
/// one canonical session/revision and revalidate the result before committing.
#[derive(Clone, Debug)]
pub struct UciPosition {
    pub root_fen: String,
    pub moves: Vec<String>,
}

impl UciPosition {
    fn validate(&self) -> Result<(), UciError> {
        if has_control(&self.root_fen)
            || self.root_fen.len() > 256
            || self.root_fen.split_ascii_whitespace().count() != 6
        {
            return Err(UciError::Invalid("expected six-field root FEN"));
        }
        if self.moves.len() > 4096 || self.moves.iter().any(|mv| !uci_move_shape(mv)) {
            return Err(UciError::Invalid("invalid UCI move history"));
        }
        Ok(())
    }

    fn command(&self) -> String {
        let mut result = format!("position fen {}", self.root_fen);
        if !self.moves.is_empty() {
            result.push_str(" moves ");
            result.push_str(&self.moves.join(" "));
        }
        result
    }
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

fn uci_move_shape(mv: &str) -> bool {
    let b = mv.as_bytes();
    (b.len() == 4 || b.len() == 5)
        && (b'a'..=b'h').contains(&b[0])
        && (b'1'..=b'8').contains(&b[1])
        && (b'a'..=b'h').contains(&b[2])
        && (b'1'..=b'8').contains(&b[3])
        && (b.len() == 4 || matches!(b[4], b'q' | b'r' | b'b' | b'n'))
}

/// Runs external UCI engines without a Timeline lock. The returned UCI string
/// is untrusted and must pass legal-move and stale-revision checks in the owner.
#[derive(Clone, Debug)]
pub struct UciEngine {
    config: UciConfig,
}

impl UciEngine {
    pub fn new(config: UciConfig) -> Self {
        Self { config }
    }

    pub async fn best_move(
        &self,
        position: &UciPosition,
        cancelled: &CancellationToken,
    ) -> Result<String, UciError> {
        self.config.validate()?;
        position.validate()?;
        if cancelled.is_cancelled() {
            return Err(UciError::Cancelled);
        }
        let mut child = Command::new(&self.config.executable)
            .args(&self.config.args)
            .envs(self.config.environment.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(UciError::Spawn)?;
        let Some(mut input) = child.stdin.take() else {
            reap(&mut child).await;
            return Err(UciError::Protocol("engine stdin missing"));
        };
        let Some(stdout) = child.stdout.take() else {
            reap(&mut child).await;
            return Err(UciError::Protocol("engine stdout missing"));
        };
        // AsyncReadExt::take bounds even an unterminated line.
        let mut output = BufReader::new(stdout.take(MAX_OUTPUT_BYTES)).lines();
        let result = self.exchange(&mut input, &mut output, position, cancelled).await;
        // A blocked/broken child is reaped regardless of protocol result.
        let _ = timeout(REAP_GRACE, input.write_all(b"quit\n")).await;
        drop(input);
        reap(&mut child).await;
        result
    }

    async fn exchange<R: AsyncBufRead + Unpin>(
        &self,
        input: &mut ChildStdin,
        output: &mut Lines<R>,
        position: &UciPosition,
        cancelled: &CancellationToken,
    ) -> Result<String, UciError> {
        send(input, "uci", cancelled).await?;
        let mut seen = 0;
        read_until(output, "uciok", "handshake",
            Instant::now() + self.config.startup_timeout, &mut seen, cancelled).await?;
        for (name, value) in &self.config.options {
            send(input, &format!("setoption name {name} value {value}"), cancelled).await?;
        }
        send(input, "isready", cancelled).await?;
        read_until(output, "readyok", "readiness",
            Instant::now() + self.config.startup_timeout, &mut seen, cancelled).await?;
        send(input, "ucinewgame", cancelled).await?;
        send(input, &position.command(), cancelled).await?;
        send(input, &format!("go movetime {}", self.config.move_time.as_millis()),
            cancelled).await?;
        let deadline = Instant::now() + self.config.search_timeout;
        loop {
            let line = next_line(output, "search", deadline, &mut seen, cancelled).await?;
            if let Some(rest) = line.strip_prefix("bestmove ") {
                let mv = rest.split_ascii_whitespace().next()
                    .ok_or(UciError::Protocol("empty bestmove"))?;
                if mv == "0000" || mv == "(none)" {
                    return Err(UciError::Protocol("engine has no playable move"));
                }
                if !uci_move_shape(mv) {
                    return Err(UciError::Protocol("invalid bestmove"));
                }
                return Ok(mv.to_owned());
            }
        }
    }
}

async fn send(
    input: &mut ChildStdin,
    command: &str,
    cancelled: &CancellationToken,
) -> Result<(), UciError> {
    if has_control(command) {
        return Err(UciError::Invalid("control character in UCI command"));
    }
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Err(UciError::Cancelled),
        result = async {
            input.write_all(command.as_bytes()).await?;
            input.write_all(b"\n").await?;
            input.flush().await
        } => result.map_err(UciError::Io),
    }
}

async fn next_line<R: AsyncBufRead + Unpin>(
    output: &mut Lines<R>,
    stage: &'static str,
    deadline: Instant,
    seen: &mut usize,
    cancelled: &CancellationToken,
) -> Result<String, UciError> {
    if *seen >= MAX_OUTPUT_LINES {
        return Err(UciError::Protocol("engine output exceeded line budget"));
    }
    let read = tokio::select! {
        biased;
        _ = cancelled.cancelled() => return Err(UciError::Cancelled),
        read = timeout_at(deadline, output.next_line()) => read,
    };
    let line = read.map_err(|_| UciError::Timeout(stage))??
        .ok_or(UciError::Protocol("engine closed stdout"))?;
    *seen += 1;
    Ok(line.trim().to_owned())
}

async fn read_until<R: AsyncBufRead + Unpin>(
    output: &mut Lines<R>,
    marker: &str,
    stage: &'static str,
    deadline: Instant,
    seen: &mut usize,
    cancelled: &CancellationToken,
) -> Result<(), UciError> {
    loop {
        if next_line(output, stage, deadline, seen, cancelled).await? == marker {
            return Ok(());
        }
    }
}

async fn reap(child: &mut Child) {
    if timeout(REAP_GRACE, child.wait()).await.is_err() {
        let _ = child.start_kill();
        let _ = timeout(Duration::from_secs(2), child.wait()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, BufRead, Write};
    const START: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

    #[test]
    fn fixture_engine() {
        let Ok(mode) = std::env::var("NEUROMITA_TEST_UCI_CHILD") else {
            return;
        };
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            match line.as_str() {
                "uci" => println!("id name fixture\nuciok"),
                "isready" => println!("readyok"),
                command if command.starts_with("go ") => match mode.as_str() {
                    "hang" => std::thread::sleep(Duration::from_secs(30)),
                    "noisy" => {
                        for depth in 0..300 {
                            println!("info depth {depth}");
                        }
                        println!("bestmove e2e4");
                    }
                    "bad" => println!("bestmove z9z9"),
                    _ => println!("bestmove e2e4 ponder e7e5"),
                },
                "quit" => break,
                _ => {}
            }
            io::stdout().flush().unwrap();
        }
    }

    fn config(mode: &str) -> UciConfig {
        let mut config = UciConfig::new(std::env::current_exe().unwrap());
        config.args = vec![
            "--exact".into(),
            "games::chess::uci::tests::fixture_engine".into(),
            "--nocapture".into(),
        ];
        config.environment = vec![("NEUROMITA_TEST_UCI_CHILD".into(), mode.into())];
        config.startup_timeout = Duration::from_secs(2);
        config.search_timeout = Duration::from_millis(500);
        config.options = vec![("Threads".into(), "1".into())];
        config
    }

    fn position() -> UciPosition {
        UciPosition { root_fen: START.into(), moves: vec!["e2e4".into()] }
    }

    #[test]
    fn rejects_invalid_input_before_launch() {
        for input in ["e2e4\nquit", "e7e8k", "a0a1", "a1a9", ""] {
            assert!(!uci_move_shape(input));
        }
        assert!(uci_move_shape("e7e8q"));
        assert!(UciPosition { root_fen: "bad fen".into(), moves: vec![] }
            .validate().is_err());
    }

    #[tokio::test]
    async fn real_child_handshake_and_bestmove() {
        let move_ = UciEngine::new(config("ok"))
            .best_move(&position(), &CancellationToken::new()).await.unwrap();
        assert_eq!(move_, "e2e4");
    }

    #[tokio::test]
    async fn rejects_malformed_or_noisy_child() {
        let bad = UciEngine::new(config("bad"))
            .best_move(&position(), &CancellationToken::new()).await;
        assert!(matches!(bad, Err(UciError::Protocol(_))));
        let noisy = UciEngine::new(config("noisy"))
            .best_move(&position(), &CancellationToken::new()).await;
        assert!(matches!(noisy, Err(UciError::Protocol(_))));
    }

    #[tokio::test]
    async fn times_out_hung_child_and_reaps_it() {
        let result = UciEngine::new(config("hang"))
            .best_move(&position(), &CancellationToken::new()).await;
        assert!(matches!(result, Err(UciError::Timeout("search"))));
    }

    #[tokio::test]
    async fn cancellation_is_explicit() {
        let token = CancellationToken::new();
        token.cancel();
        let result = UciEngine::new(config("ok")).best_move(&position(), &token).await;
        assert!(matches!(result, Err(UciError::Cancelled)));
    }

    #[tokio::test]
    async fn missing_engine_is_reported() {
        let result = UciEngine::new(UciConfig::new(
            "neuromita-uci-definitely-not-installed-777"
        )).best_move(&position(), &CancellationToken::new()).await;
        assert!(matches!(result, Err(UciError::Spawn(_))));
    }
}

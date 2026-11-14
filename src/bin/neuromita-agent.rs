#[tokio::main]
async fn main() -> std::process::ExitCode {
    match neuromita::agent_ipc::run(std::env::args().skip(1)).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("agent IPC: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

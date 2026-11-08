use neuromita::{
    ai_runtime::{AiService, runtime_status::RuntimePhase},
    app::ApplicationLifecycle,
};

enum AiWorkerCommand {
    Status,
    Restart(AiService, uuid::Uuid),
}

fn parse_ai_worker_command(input: &str) -> Result<AiWorkerCommand, &'static str> {
    let words: Vec<_> = input.split_whitespace().collect();
    match words.as_slice() {
        ["/ai", "worker", "status"] => Ok(AiWorkerCommand::Status),
        ["/ai", "worker", "restart", service, generation] => {
            let service = match *service {
                "Embedding" => AiService::Embedding,
                "Reranker" => AiService::Reranker,
                "Tts" => AiService::Tts,
                "Asr" => AiService::Asr,
                _ => return Err("unknown AI service"),
            };
            let generation = uuid::Uuid::parse_str(generation)
                .map_err(|_| "expected generation must be a UUID from /ai worker status")?;
            Ok(AiWorkerCommand::Restart(service, generation))
        }
        _ => Err(
            "usage: /ai worker status | /ai worker restart Embedding|Reranker|Tts|Asr EXPECTED_GENERATION_UUID",
        ),
    }
}

pub(super) async fn run(app: &ApplicationLifecycle, line: &str) -> Result<String, String> {
    match parse_ai_worker_command(line).map_err(str::to_owned)? {
        AiWorkerCommand::Status => {
            let snapshot = app.presenter().ai_runtime.refresh_status().await;
            let mut lines = Vec::new();
            for service in [
                AiService::Embedding,
                AiService::Reranker,
                AiService::Tts,
                AiService::Asr,
            ] {
                let status = snapshot.service(service);
                let generation = status
                    .generation
                    .map_or_else(|| "none".to_owned(), |id| id.to_string());
                let model = status
                    .active_model
                    .as_ref()
                    .map_or("none", |model| model.plugin_id.as_str());
                lines.push(format!(
                    "{service:?}: {:?}; generation={generation}; model={model}; error={}",
                    status.phase,
                    status.last_error.as_deref().unwrap_or("none"),
                ));
            }
            Ok(lines.join("\n"))
        }
        AiWorkerCommand::Restart(service, expected_generation) => {
            let generation = app
                .restart_failed_ai_worker(service, expected_generation)
                .await
                .map_err(|error| error.to_string())?;
            let snapshot = app.presenter().ai_runtime.refresh_status().await;
            let status = snapshot.service(service);
            if status.phase != RuntimePhase::ModelReady || status.generation != Some(generation) {
                return Err("worker restart returned an inconsistent runtime status".into());
            }
            Ok(format!("{service:?}: ModelReady; generation={generation}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_worker_command_requires_exact_service_and_generation() {
        let generation = uuid::Uuid::new_v4();
        assert!(matches!(
            parse_ai_worker_command("/ai worker status"),
            Ok(AiWorkerCommand::Status)
        ));
        assert!(matches!(
            parse_ai_worker_command(&format!("/ai worker restart Embedding {generation}")),
            Ok(AiWorkerCommand::Restart(AiService::Embedding, id)) if id == generation
        ));
        for input in [
            "/ai worker restart Embedding",
            "/ai worker restart embedding 01234567-89ab-4cde-8123-456789abcdef",
            "/ai worker restart Embedding 01234567-89ab-4cde-8123-456789abcdef extra",
            "/ai worker restart Embedding not-a-uuid",
            "/ai worker restart All 01234567-89ab-4cde-8123-456789abcdef",
            "/ai worker status extra",
        ] {
            assert!(parse_ai_worker_command(input).is_err(), "{input}");
        }
    }
}

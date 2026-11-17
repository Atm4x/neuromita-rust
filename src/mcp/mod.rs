mod handler;

pub use handler::run_stdio;
use http_body_util::BodyExt;
use hyper::{Request, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::watch, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tower_service::Service;

#[derive(Debug, Clone, Default, Serialize)]
pub struct McpStatus {
    pub active: bool,
    pub endpoint: String,
    pub error: Option<String>,
}

pub struct McpRuntime {
    status: watch::Receiver<McpStatus>,
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl McpRuntime {
    pub fn start(
        core: std::net::SocketAddr,
        mut settings: watch::Receiver<Arc<crate::settings::SettingsSnapshot>>,
        stop: CancellationToken,
    ) -> Self {
        let (send, status) = watch::channel(McpStatus::default());
        let stop = stop.child_token();
        let token = stop.clone();
        let task = tokio::spawn(async move {
            let mut current = None;
            let mut listener_task: Option<(
                CancellationToken,
                tokio::task::JoinHandle<std::io::Result<()>>,
            )> = None;
            loop {
                let configured = settings.borrow_and_update().values.mcp.clone();
                if current.as_ref() != Some(&configured) {
                    if let Some((cancel, task)) = listener_task.take() {
                        cancel.cancel();
                        let _ = task.await;
                    }
                    let mut state = McpStatus {
                        endpoint: configured.endpoint.clone(),
                        ..Default::default()
                    };
                    if configured.enabled {
                        match configured.address() {
                            Ok((address, path)) => match TcpListener::bind(address).await {
                                Ok(listener) => {
                                    let cancel = token.child_token();
                                    let worker_stop = cancel.clone();
                                    listener_task = Some((
                                        cancel,
                                        tokio::spawn(serve(
                                            listener,
                                            path,
                                            core,
                                            configured.clone(),
                                            worker_stop,
                                        )),
                                    ));
                                    state.active = true;
                                }
                                Err(error) => state.error = Some(error.to_string()),
                            },
                            Err(error) => state.error = Some(error.to_string()),
                        }
                    }
                    send.send_replace(state);
                    current = Some(configured.clone());
                }
                tokio::select! {
                    _ = token.cancelled() => break,
                    changed = settings.changed() => if changed.is_err() { break; },
                    outcome = async { match listener_task.as_mut() {
                        Some((_, task)) => task.await,
                        None => std::future::pending().await,
                    }} => {
                        if let Some((cancel, _)) = listener_task.take() { cancel.cancel(); }
                        let error = match outcome {
                            Ok(Err(error)) => error.to_string(),
                            Err(error) => error.to_string(),
                            Ok(Ok(())) => "MCP listener stopped".into(),
                        };
                        send.send_replace(McpStatus { active: false,
                            endpoint: configured.endpoint, error: Some(error) });
                    },
                }
            }
            if let Some((cancel, task)) = listener_task {
                cancel.cancel();
                let _ = task.await;
            }
            send.send_replace(McpStatus::default());
        });
        Self {
            status,
            stop,
            task: Some(task),
        }
    }

    pub async fn shutdown(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    pub fn observe(&self) -> watch::Receiver<McpStatus> {
        self.status.clone()
    }
}

impl Drop for McpRuntime {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn serve(
    listener: TcpListener,
    path: String,
    core: std::net::SocketAddr,
    settings: crate::settings::McpSettings,
    stop: CancellationToken,
) -> std::io::Result<()> {
    let mut config = StreamableHttpServerConfig::default().with_json_response(true);
    config.cancellation_token = stop.clone();
    config = config.enforce_origin_validation();
    let service = StreamableHttpService::new(
        move || {
            Ok(handler::NeuroMita {
                core,
                allow_write: settings.allow_write,
                allow_content: settings.allow_content,
                core_debug: true,
                timeout: Duration::from_secs(60),
            })
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            accepted = listener.accept(), if clients.len() < 32 => {
                let (socket, _) = accepted?;
                let endpoint = path.clone();
                let service = service.clone();
                let cancel = stop.clone();
                clients.spawn(async move {
                    let handler = service_fn(move |request: Request<Incoming>| {
                        let mut service = service.clone();
                        let endpoint = endpoint.clone();
                        async move {
                            if request.uri().path() != endpoint {
                                let body = http_body_util::Empty::<bytes::Bytes>::new().map_err(|never| match never {}).boxed();
                                let mut response = hyper::Response::new(body);
                                *response.status_mut() = hyper::StatusCode::NOT_FOUND;
                                return Ok::<_, std::convert::Infallible>(response);
                            }
                            service.call(request).await
                        }
                    });
                    tokio::select! {
                        _ = cancel.cancelled() => {},
                        _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(socket), handler) => {},
                    }
                });
            }
        }
    }
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    Ok(())
}

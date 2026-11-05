//! Game-origin command and result channel. A session survives TCP reconnects for
//! the lifetime of the application; request state is owned by RequestHandle.
use crate::{
    character::CharacterId,
    identity::EventId,
    request::{
        InitiateRequest, ReactRequest, ReplyRequest, RequestHandle, RequestSource, RequestState,
    },
    runtime::ApiBindings,
    timeline::conversation::{ConversationAudience, ConversationMessage, ConversationSpeaker},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct GameLinkConfig {
    pub bind: SocketAddr,
    pub max_command_bytes: usize,
    pub outbound_capacity: usize,
    pub max_sessions: usize,
    pub max_requests_per_session: usize,
}
impl GameLinkConfig {
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            max_command_bytes: 65536,
            outbound_capacity: 64,
            max_sessions: 16,
            max_requests_per_session: 128,
        }
    }
}

#[derive(Clone)]
pub struct GameLinkHandle {
    address: SocketAddr,
}
impl GameLinkHandle {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

struct Session {
    token: Uuid,
    connection: Option<(Uuid, mpsc::Sender<Value>)>,
    receipts: HashMap<Uuid, (Value, Receipt)>,
}
#[derive(Clone)]
enum Receipt {
    Request(RequestHandle),
    Published(EventId),
}

#[derive(Default)]
struct Sessions(Mutex<HashMap<Uuid, Session>>);
impl Sessions {
    fn create(
        &self,
        connection: Uuid,
        outbound: mpsc::Sender<Value>,
        max: usize,
    ) -> Result<(Uuid, Uuid), &'static str> {
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        if sessions.len() >= max {
            return Err("game session capacity reached");
        }
        let id = Uuid::new_v4();
        let token = Uuid::new_v4();
        sessions.insert(
            id,
            Session {
                token,
                connection: Some((connection, outbound)),
                receipts: HashMap::new(),
            },
        );
        Ok((id, token))
    }
    fn resume(
        &self,
        id: Uuid,
        token: Uuid,
        connection: Uuid,
        outbound: mpsc::Sender<Value>,
    ) -> Result<Vec<RequestHandle>, &'static str> {
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get_mut(&id).ok_or("unknown session")?;
        if session.token != token {
            return Err("invalid session credential");
        }
        session.connection = Some((connection, outbound));
        Ok(session
            .receipts
            .values()
            .filter_map(|(_, receipt)| match receipt {
                Receipt::Request(handle) => Some(handle.clone()),
                _ => None,
            })
            .collect())
    }
    fn clear(&self, id: Uuid, connection: Uuid) {
        if let Some(session) = self.0.lock().expect("game sessions poisoned").get_mut(&id)
            && session
                .connection
                .as_ref()
                .is_some_and(|(active, _)| *active == connection)
        {
            session.connection = None;
        }
    }
    fn receipt(&self, id: Uuid, command: Uuid) -> Option<(Value, Receipt)> {
        self.0
            .lock()
            .expect("game sessions poisoned")
            .get(&id)?
            .receipts
            .get(&command)
            .cloned()
    }
    fn record(
        &self,
        id: Uuid,
        command: Uuid,
        fingerprint: Value,
        result: Receipt,
        max: usize,
    ) -> Result<(), &'static str> {
        let mut sessions = self.0.lock().expect("game sessions poisoned");
        let session = sessions.get_mut(&id).ok_or("session not found")?;
        if session.receipts.len() >= max {
            if let Some(old) =
                session
                    .receipts
                    .iter()
                    .find_map(|(id, (_, receipt))| match receipt {
                        Receipt::Published(_) => Some(*id),
                        Receipt::Request(handle) if handle.snapshot().state.is_terminal() => {
                            Some(*id)
                        }
                        _ => None,
                    })
            {
                session.receipts.remove(&old);
            } else {
                return Err("session request capacity reached");
            }
        }
        session.receipts.insert(command, (fingerprint, result));
        Ok(())
    }
    async fn push(&self, id: Uuid, frame: Value) {
        let output = self
            .0
            .lock()
            .expect("game sessions poisoned")
            .get(&id)
            .and_then(|session| session.connection.as_ref().map(|(_, tx)| tx.clone()));
        if let Some(output) = output {
            let _ = output.send(frame).await;
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum GameCommand {
    Hello {},
    Resume {
        session_id: Uuid,
        session_token: Uuid,
    },
    Ping {},
    PublishMessage {
        event_id: Uuid,
        occurred_at_unix_ms: u64,
        content: Value,
        character: CharacterId,
    },
    Reply {
        character: CharacterId,
        message_id: Uuid,
    },
    React {
        character: CharacterId,
        event_id: Uuid,
        #[serde(default)]
        instruction: String,
    },
    Initiate {
        character: CharacterId,
        #[serde(default)]
        instruction: String,
    },
    Cancel {
        request_id: Uuid,
    },
}
#[derive(Deserialize)]
struct Frame {
    command_id: Uuid,
    #[serde(flatten)]
    command: GameCommand,
}

fn error(command_id: Option<Uuid>, code: &str, reason: impl std::fmt::Display) -> Value {
    json!({"type":"error", "command_id":command_id, "code":code, "error":reason.to_string()})
}
fn request_frame(snapshot: &crate::request::RequestSnapshot, session_id: Uuid) -> Value {
    let mut frame = crate::server::status(snapshot);
    frame["session_id"] = json!(session_id);
    frame["type"] = json!(match &snapshot.state {
        RequestState::Cancelled => "request_cancelled",
        RequestState::Finished(
            crate::request::RequestOutcome::TextGenerationFailed(_)
            | crate::request::RequestOutcome::InternalError(_),
        ) => "request_failed",
        RequestState::Finished(_) => "request_completed",
        _ => "request_updated",
    });
    frame
}

pub struct GameLinkLifecycle {
    listener: TcpListener,
    config: GameLinkConfig,
    sessions: Arc<Sessions>,
}
impl GameLinkLifecycle {
    pub async fn bind(config: GameLinkConfig) -> io::Result<(Self, GameLinkHandle)> {
        let listener = TcpListener::bind(config.bind).await?;
        let address = listener.local_addr()?;
        Ok((
            Self {
                listener,
                config,
                sessions: Arc::new(Sessions::default()),
            },
            GameLinkHandle { address },
        ))
    }
    pub(crate) async fn run_with_api(
        self,
        stop: CancellationToken,
        api: Arc<ApiBindings>,
    ) -> io::Result<()> {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                result = connections.join_next(), if !connections.is_empty() => { let _ = result; },
                accepted = self.listener.accept() => {
                    let (stream,_) = accepted?;
                    let sessions = Arc::clone(&self.sessions);
                    let api = Arc::clone(&api);
                    let config = self.config;
                    let connection_stop = stop.child_token();
                    connections.spawn(async move { let _ = client(stream,config,sessions,api,connection_stop).await; });
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Ok(())
    }
}

async fn client(
    stream: TcpStream,
    config: GameLinkConfig,
    sessions: Arc<Sessions>,
    api: Arc<ApiBindings>,
    stop: CancellationToken,
) -> io::Result<()> {
    let connection = Uuid::new_v4();
    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<Value>(config.outbound_capacity.max(1));
    let writer_stop = stop.clone();
    let writer = tokio::spawn(async move {
        loop {
            let frame = tokio::select! { biased; _ = writer_stop.cancelled() => break, value=rx.recv()=> match value {Some(value)=>value,None=>break} };
            let mut bytes = serde_json::to_vec(&frame).map_err(io::Error::other)?;
            bytes.push(b'\n');
            write.write_all(&bytes).await?;
        }
        Ok::<(), io::Error>(())
    });
    let mut lines = BufReader::new(read).lines();
    let mut session_id = None;
    let result = async {
        loop {
            let line = tokio::select! { biased; _ = stop.cancelled()=>break, incoming=lines.next_line()=>match incoming?{Some(line)=>line,None=>break} };
            if line.len() > config.max_command_bytes { tx.send(error(None,"command_too_large","command exceeds configured limit")).await.map_err(io::Error::other)?; break; }
            let frame: Frame = match serde_json::from_str(&line) {
                Ok(frame) => frame,
                Err(e) => { tx.send(error(None,"invalid_command",e)).await.map_err(io::Error::other)?; continue; }
            };
            let id = frame.command_id;
            match frame.command {
                GameCommand::Hello {} => {
                    if session_id.is_some() { tx.send(error(Some(id),"already_connected","session already established")).await.map_err(io::Error::other)?; continue; }
                    match sessions.create(connection,tx.clone(),config.max_sessions) {
                        Ok((session,token)) => { session_id=Some(session); tx.send(json!({"type":"hello_ack","command_id":id,"session_id":session,"session_token":token})).await.map_err(io::Error::other)?; },
                        Err(e)=>tx.send(error(Some(id),"capacity",e)).await.map_err(io::Error::other)?,
                    }
                }
                GameCommand::Resume {session_id: target, session_token} => {
                    if session_id.is_some() { tx.send(error(Some(id),"already_connected","session already established")).await.map_err(io::Error::other)?; continue; }
                    match sessions.resume(target,session_token,connection,tx.clone()) {
                        Err(e) => tx.send(error(Some(id),"session_denied",e)).await.map_err(io::Error::other)?,
                        Ok(handles) => {
                            session_id=Some(target);
                            tx.send(json!({"type":"session_resumed","command_id":id,"session_id":target,"requests":handles.len()})).await.map_err(io::Error::other)?;
                            for handle in handles { tx.send(request_frame(&handle.snapshot(),target)).await.map_err(io::Error::other)?; }
                        }
                    }
                }
                GameCommand::Ping {} => { tx.send(json!({"type":"pong","command_id":id})).await.map_err(io::Error::other)?; }
                command => {
                    let Some(session) = session_id else { tx.send(error(Some(id),"hello_required","establish or resume game session first")).await.map_err(io::Error::other)?; continue; };
                    let fingerprint = serde_json::to_value(&command).map_err(io::Error::other)?;
                    if let Some((previous,receipt)) = sessions.receipt(session,id) {
                        if previous != fingerprint { tx.send(error(Some(id),"command_id_conflict","command ID has different payload")).await.map_err(io::Error::other)?; }
                        else { match receipt {
                            Receipt::Published(event)=> tx.send(json!({"type":"published","command_id":id,"event_id":event.0,"replayed":true})).await.map_err(io::Error::other)?,
                            Receipt::Request(handle)=> { tx.send(json!({"type":"request_accepted","command_id":id,"request_id":handle.id().0,"replayed":true})).await.map_err(io::Error::other)?; tx.send(request_frame(&handle.snapshot(),session)).await.map_err(io::Error::other)?; }
                        }}
                        continue;
                    }
                    match command {
                        GameCommand::PublishMessage {event_id,occurred_at_unix_ms,content,character} => {
                            let mut message = ConversationMessage::new(event_id.to_string(),ConversationSpeaker::Player,content,[character]);
                            message.id=EventId(event_id);
                            message.occurred_at_unix_ms=occurred_at_unix_ms;
                            message.audience=ConversationAudience::player_to(character);
                            match api.chat.publish_conversation_message(message) {
                                Err(e)=>tx.send(error(Some(id),"publish_failed",e)).await.map_err(io::Error::other)?,
                                Ok(event)=>match sessions.record(session,id,fingerprint,Receipt::Published(event),config.max_requests_per_session) {
                                    Err(e)=>tx.send(error(Some(id),"capacity",e)).await.map_err(io::Error::other)?,
                                    Ok(())=>tx.send(json!({"type":"published","command_id":id,"event_id":event.0,"replayed":false})).await.map_err(io::Error::other)?,
                                }
                            }
                        }
                        GameCommand::Reply {character,message_id} | GameCommand::React {character,event_id:message_id,instruction:_} => {
                            let source = RequestSource::Game(session);
                            let result = match command {
                                GameCommand::Reply {..} => api.chat.send(ReplyRequest::new(character,EventId(message_id),source)),
                                GameCommand::React {instruction,..} => api.chat.send(ReactRequest::new(character,EventId(message_id),instruction,source)),
                                _=> unreachable!(),
                            };
                            accept_request(result,session,id,fingerprint,config.max_requests_per_session,&sessions,&tx).await?;
                        }
                        GameCommand::Initiate {character,instruction} => {
                            accept_request(api.chat.send(InitiateRequest::new(character,instruction,RequestSource::Game(session))),session,id,fingerprint,config.max_requests_per_session,&sessions,&tx).await?;
                        }
                        GameCommand::Cancel {request_id} => {
                            let handle = sessions.0.lock().expect("game sessions poisoned").get(&session).and_then(|s| s.receipts.values().find_map(|(_,receipt)| match receipt {Receipt::Request(h) if h.id().0==request_id=>Some(h.clone()),_=>None}));
                            match handle {
                                Some(h)=>tx.send(json!({"type":"cancel_ack","command_id":id,"request_id":request_id,"result":format!("{:?}",h.cancel())})).await.map_err(io::Error::other)?,
                                None=>tx.send(error(Some(id),"ownership_denied","request is not owned by game session")).await.map_err(io::Error::other)?,
                            }
                        }
                        _=>unreachable!(),
                    }
                }
            }
        }
        Ok::<(),io::Error>(())
    }.await;
    if let Some(session) = session_id {
        sessions.clear(session, connection);
    }
    stop.cancel();
    drop(tx);
    let _ = writer.await;
    result
}

async fn accept_request(
    result: Result<RequestHandle, crate::request::SubmitError>,
    session: Uuid,
    id: Uuid,
    fingerprint: Value,
    capacity: usize,
    sessions: &Arc<Sessions>,
    tx: &mpsc::Sender<Value>,
) -> io::Result<()> {
    let handle = match result {
        Ok(h) => h,
        Err(e) => {
            tx.send(error(Some(id), "request_rejected", e))
                .await
                .map_err(io::Error::other)?;
            return Ok(());
        }
    };
    if let Err(e) = sessions.record(
        session,
        id,
        fingerprint,
        Receipt::Request(handle.clone()),
        capacity,
    ) {
        handle.cancel();
        tx.send(error(Some(id), "capacity", e))
            .await
            .map_err(io::Error::other)?;
        return Ok(());
    }
    tx.send(json!({"type":"request_accepted","command_id":id,"request_id":handle.id().0,"replayed":false})).await.map_err(io::Error::other)?;
    let sessions = Arc::clone(sessions);
    tokio::spawn(async move {
        let mut observer = handle.observe();
        while let Ok(Some(snapshot)) = observer.next().await {
            sessions
                .push(session, request_frame(&snapshot, session))
                .await;
        }
    });
    Ok(())
}

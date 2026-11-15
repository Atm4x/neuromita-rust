//! Application-owned transcript routing, shared by the GUI and local MCP administration.
//! Recognition creates one ticket-admitted event, not a second chat submission path.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::{broadcast, watch};

use super::{
    session::{SpeechConversation, SpeechFuture},
    ticket::SpeechTicket,
};
use crate::{ai_runtime::speech::Transcript, character::CharacterId, voice::AudioArtifact};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedTranscript {
    pub revision: u64,
    pub character: CharacterId,
    pub selection_revision: u64,
    pub transcript: Transcript,
}

pub struct SpeechTranscriptHub {
    revision: AtomicU64,
    stream: broadcast::Sender<RoutedTranscript>,
    latest: watch::Sender<Option<RoutedTranscript>>,
    gui_route: watch::Sender<Option<(CharacterId, u64)>>,
}

impl Default for SpeechTranscriptHub {
    fn default() -> Self {
        let (stream, _) = broadcast::channel(64);
        let (latest, _) = watch::channel(None);
        let (gui_route, _) = watch::channel(None);
        Self {
            revision: AtomicU64::new(0),
            stream,
            latest,
            gui_route,
        }
    }
}

impl SpeechTranscriptHub {
    pub fn set_gui_route(&self, character: CharacterId, selection_revision: u64) {
        self.gui_route
            .send_replace(Some((character, selection_revision)));
    }

    pub fn gui_route(&self) -> Option<(CharacterId, u64)> {
        *self.gui_route.borrow()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RoutedTranscript> {
        self.stream.subscribe()
    }

    pub fn observe(&self) -> watch::Receiver<Option<RoutedTranscript>> {
        self.latest.subscribe()
    }

    pub fn latest(&self) -> Option<RoutedTranscript> {
        self.latest.borrow().clone()
    }

    pub fn destination(
        self: &Arc<Self>,
        character: CharacterId,
        selection_revision: u64,
    ) -> Arc<dyn SpeechConversation> {
        Arc::new(RoutedDestination {
            hub: Arc::clone(self),
            character,
            selection_revision,
        })
    }

    fn publish(&self, character: CharacterId, selection_revision: u64, transcript: Transcript) {
        let event = RoutedTranscript {
            revision: self.revision.fetch_add(1, Ordering::Relaxed) + 1,
            character,
            selection_revision,
            transcript,
        };
        self.latest.send_replace(Some(event.clone()));
        let _ = self.stream.send(event);
    }
}

struct RoutedDestination {
    hub: Arc<SpeechTranscriptHub>,
    character: CharacterId,
    selection_revision: u64,
}

impl SpeechConversation for RoutedDestination {
    fn reply<'a>(
        &'a self,
        ticket: &'a SpeechTicket,
        transcript: &'a Transcript,
    ) -> SpeechFuture<'a, Option<AudioArtifact>> {
        Box::pin(async move {
            let _ = ticket.admit(|| {
                self.hub
                    .publish(self.character, self.selection_revision, transcript.clone())
            });
            Ok(None)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::ticket::SpeechStream;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn transcript_routes_once_to_shared_gui_and_observer_and_rejects_stale_ticket() {
        let hub = Arc::new(SpeechTranscriptHub::default());
        hub.set_gui_route(CharacterId::Kind, 7);
        assert_eq!(hub.gui_route(), Some((CharacterId::Kind, 7)));
        let mut gui = hub.subscribe();
        let mut observer = hub.observe();
        let stream = SpeechStream::new(CancellationToken::new());
        let destination = hub.destination(CharacterId::Kind, 7);
        let ticket = stream.ticket();
        let words = Transcript {
            text: "recognized text".into(),
            language: Some("ru".into()),
        };
        destination.reply(&ticket, &words).await.unwrap();
        let received = gui.try_recv().unwrap();
        assert_eq!(received.character, CharacterId::Kind);
        assert_eq!(received.selection_revision, 7);
        assert_eq!(received.revision, 1);
        assert_eq!(received.transcript, words);
        assert_eq!(observer.borrow_and_update().as_ref().unwrap().revision, 1);
        destination.reply(&ticket, &words).await.unwrap();
        assert!(gui.try_recv().is_err());
        stream.invalidate();
        destination.reply(&ticket, &words).await.unwrap();
        assert!(gui.try_recv().is_err());
        destination.reply(&stream.ticket(), &words).await.unwrap();
        assert_eq!(gui.try_recv().unwrap().revision, 2);
        assert_eq!(hub.latest().unwrap().revision, 2);
    }

    #[tokio::test]
    async fn cancelled_session_does_not_emit_transcript() {
        let hub = Arc::new(SpeechTranscriptHub::default());
        let stop = CancellationToken::new();
        let stream = SpeechStream::new(stop.clone());
        let ticket = stream.ticket();
        stop.cancel();
        hub.destination(CharacterId::Crazy, 0)
            .reply(
                &ticket,
                &Transcript {
                    text: "private".into(),
                    language: None,
                },
            )
            .await
            .unwrap();
        assert!(hub.latest().is_none());
    }
}

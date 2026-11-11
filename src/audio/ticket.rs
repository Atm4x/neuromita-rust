use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Generation {
    id: Uuid,
    stop: CancellationToken,
}

pub struct SpeechStream {
    stop: CancellationToken,
    generation: Mutex<Generation>,
}

impl SpeechStream {
    pub fn new(stop: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            generation: Mutex::new(Generation {
                id: Uuid::new_v4(),
                stop: stop.child_token(),
            }),
            stop,
        })
    }

    pub fn invalidate(&self) {
        let mut generation = self.generation.lock().expect("speech generation poisoned");
        generation.stop.cancel();
        *generation = Generation {
            id: Uuid::new_v4(),
            stop: self.stop.child_token(),
        };
    }

    pub fn ticket(self: &Arc<Self>) -> SpeechTicket {
        let generation = self.generation.lock().expect("speech generation poisoned");
        SpeechTicket {
            stream: Arc::clone(self),
            generation: generation.id,
            stop: generation.stop.child_token(),
            admitted: AtomicBool::new(false),
        }
    }
}

impl Drop for SpeechStream {
    fn drop(&mut self) {
        if let Ok(generation) = self.generation.lock() {
            generation.stop.cancel();
        }
    }
}

pub struct SpeechTicket {
    stream: Arc<SpeechStream>,
    generation: Uuid,
    stop: CancellationToken,
    admitted: AtomicBool,
}

impl SpeechTicket {
    pub fn cancellation(&self) -> CancellationToken {
        self.stop.clone()
    }

    pub fn is_current(&self) -> bool {
        let generation = self
            .stream
            .generation
            .lock()
            .expect("speech generation poisoned");
        generation.id == self.generation && !self.stop.is_cancelled()
    }

    pub fn admit<T>(&self, admission: impl FnOnce() -> T) -> Option<T> {
        let generation = self
            .stream
            .generation
            .lock()
            .expect("speech generation poisoned");
        if generation.id != self.generation
            || self.stop.is_cancelled()
            || self.admitted.swap(true, Ordering::AcqRel)
        {
            return None;
        }
        Some(admission())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn admission_is_once_only_and_reset_rejects_stale_tickets() {
        let stream = SpeechStream::new(CancellationToken::new());
        let ticket = stream.ticket();
        let count = AtomicUsize::new(0);
        assert!(ticket.is_current());
        assert_eq!(
            ticket.admit(|| count.fetch_add(1, Ordering::SeqCst)),
            Some(0)
        );
        assert_eq!(ticket.admit(|| count.fetch_add(1, Ordering::SeqCst)), None);
        stream.invalidate();
        assert!(!ticket.is_current());
        assert_eq!(ticket.admit(|| count.fetch_add(1, Ordering::SeqCst)), None);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let next = stream.ticket();
        assert!(next.is_current());
        assert_eq!(next.admit(|| count.fetch_add(1, Ordering::SeqCst)), Some(1));
    }

    #[test]
    fn parent_stop_cancels_pending_admission() {
        let stop = CancellationToken::new();
        let stream = SpeechStream::new(stop.clone());
        let ticket = stream.ticket();
        stop.cancel();
        assert!(!ticket.is_current());
        assert_eq!(ticket.admit(|| 12), None);
    }
}

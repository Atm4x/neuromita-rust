use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use thiserror::Error;
use tokio::time::Instant;
use uuid::Uuid;

const MAX_IDENTITIES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Automatic,
    PushToTalk,
    Radio,
}

#[derive(Debug, Clone, Copy)]
pub struct GateConfig {
    pub mode: InputMode,
    pub enabled: bool,
    pub pause_while_speaking: bool,
    pub manual_lease: Duration,
    pub speaking_tail: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct ManualCapture {
    pub session: Uuid,
    pub generation: u64,
    pub active: bool,
    pub cancelled: bool,
    pub renew: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum GateCommand {
    SetEnabled(bool),
    SetMode(InputMode),
    Manual(ManualCapture),
    SpeakingStarted {
        source: Uuid,
        utterance: Uuid,
        sequence: u64,
        lease: Duration,
    },
    SpeakingStopped {
        source: Uuid,
        utterance: Uuid,
        sequence: u64,
    },
    SourceDisconnected(Uuid),
    Reset,
}

#[derive(Debug, Clone, Copy)]
pub struct GateSnapshot {
    pub mode: InputMode,
    pub permitted: bool,
    pub allowed: bool,
    pub manual_active: bool,
    pub epoch: u64,
    pub needs_release: bool,
}

#[derive(Debug, Error)]
pub enum GateError {
    #[error("invalid capture gate timing")]
    Timing,
    #[error("capture gate identity capacity exhausted; restart the stream")]
    Capacity,
}

struct Press {
    session: Uuid,
    generation: u64,
    until: Option<Instant>,
    needs_release: bool,
}

/// Host events are ordered by the capture owner. Retired identities remain
/// fenced until restart; exhaustion is reported rather than evicting a fence.
pub struct InputGate {
    config: GateConfig,
    press: Option<Press>,
    retired: HashSet<Uuid>,
    speaking: HashMap<(Uuid, Uuid), Instant>,
    speaking_sequence: HashMap<Uuid, u64>,
    tail: Option<Instant>,
    epoch: u64,
}

impl InputGate {
    pub fn new(config: GateConfig) -> Result<Self, GateError> {
        if config.manual_lease.is_zero()
            || config.manual_lease > Duration::from_secs(10)
            || config.speaking_tail > Duration::from_secs(2)
        {
            return Err(GateError::Timing);
        }
        Ok(Self {
            config,
            press: None,
            retired: HashSet::new(),
            speaking: HashMap::new(),
            speaking_sequence: HashMap::new(),
            tail: None,
            epoch: 0,
        })
    }

    fn cancel(&mut self) {
        if let Some(press) = &mut self.press
            && press.until.take().is_some()
        {
            press.needs_release = true;
        }
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub fn snapshot(&mut self, now: Instant) -> GateSnapshot {
        if self
            .press
            .as_ref()
            .and_then(|p| p.until)
            .is_some_and(|end| end <= now)
        {
            self.cancel();
        }
        self.speaking.retain(|_, end| {
            if *end > now {
                true
            } else {
                self.tail = self.tail.max(Some(*end + self.config.speaking_tail));
                false
            }
        });
        if self.tail.is_some_and(|end| end <= now) {
            self.tail = None;
        }
        let permitted = self.config.enabled
            && (!self.config.pause_while_speaking
                || (self.tail.is_none() && self.speaking.is_empty()));
        let manual_active = self.press.as_ref().is_some_and(|p| p.until.is_some());
        GateSnapshot {
            mode: self.config.mode,
            permitted,
            allowed: permitted && (self.config.mode == InputMode::Automatic || manual_active),
            manual_active,
            epoch: self.epoch,
            needs_release: self.press.as_ref().is_some_and(|p| p.needs_release),
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.speaking
            .values()
            .copied()
            .chain(self.tail)
            .chain(self.press.as_ref().and_then(|p| p.until))
            .min()
    }

    pub fn apply(&mut self, command: GateCommand, now: Instant) -> Result<(), GateError> {
        let before = self.snapshot(now);
        match command {
            GateCommand::SetEnabled(enabled) => self.config.enabled = enabled,
            GateCommand::SetMode(mode) if mode != self.config.mode => {
                self.config.mode = mode;
                self.cancel();
            }
            GateCommand::SetMode(_) => {}
            GateCommand::Reset => self.cancel(),
            GateCommand::Manual(event) => self.manual(event, before.permitted, now),
            GateCommand::SourceDisconnected(source) => {
                if self.retired.len() == MAX_IDENTITIES && !self.retired.contains(&source) {
                    return Err(GateError::Capacity);
                }
                self.retired.insert(source);
                if self.press.as_ref().is_some_and(|p| p.session == source) {
                    self.cancel();
                    self.press = None;
                }
                let mut ended = false;
                self.speaking_sequence.remove(&source);
                self.speaking.retain(|(owner, _), _| {
                    if *owner == source {
                        ended = true;
                        false
                    } else {
                        true
                    }
                });
                if ended {
                    self.tail = self.tail.max(Some(now + self.config.speaking_tail));
                }
            }
            GateCommand::SpeakingStarted {
                source,
                utterance,
                sequence,
                lease,
            } => {
                if lease.is_zero() || lease > Duration::from_secs(180) {
                    return Err(GateError::Timing);
                }
                let key = (source, utterance);
                if self.accept_speaking(source, sequence)? {
                    if self.speaking.len() == MAX_IDENTITIES && !self.speaking.contains_key(&key) {
                        return Err(GateError::Capacity);
                    }
                    self.speaking.insert(key, now + lease);
                }
            }
            GateCommand::SpeakingStopped {
                source,
                utterance,
                sequence,
            } => {
                if self.accept_speaking(source, sequence)?
                    && self.speaking.remove(&(source, utterance)).is_some()
                {
                    self.tail = self.tail.max(Some(now + self.config.speaking_tail));
                }
            }
        }
        if before.permitted && !self.snapshot(now).permitted {
            self.cancel();
        }
        Ok(())
    }

    fn accept_speaking(&mut self, source: Uuid, sequence: u64) -> Result<bool, GateError> {
        if self.retired.contains(&source)
            || self
                .speaking_sequence
                .get(&source)
                .is_some_and(|old| sequence <= *old)
        {
            return Ok(false);
        }
        if self.speaking_sequence.len() == MAX_IDENTITIES
            && !self.speaking_sequence.contains_key(&source)
        {
            return Err(GateError::Capacity);
        }
        self.speaking_sequence.insert(source, sequence);
        Ok(true)
    }

    fn manual(&mut self, event: ManualCapture, permitted: bool, now: Instant) {
        if self.retired.contains(&event.session)
            || (self.config.mode == InputMode::Automatic && event.active)
        {
            return;
        }
        if self.press.is_none() && event.active && !event.renew && !event.cancelled {
            self.press = Some(Press {
                session: event.session,
                generation: event.generation,
                until: None,
                needs_release: !permitted,
            });
            if permitted {
                self.press.as_mut().expect("inserted press").until =
                    Some(now + self.config.manual_lease);
                self.epoch = self.epoch.wrapping_add(1);
            }
            return;
        }
        let Some(press) = &mut self.press else { return };
        if press.session != event.session || event.generation < press.generation {
            return;
        }
        if event.cancelled {
            press.generation = event.generation;
            press.until = None;
            press.needs_release = event.active;
            self.epoch = self.epoch.wrapping_add(1);
        } else if !event.active && !event.renew && event.generation == press.generation {
            press.until = None;
            press.needs_release = false;
        } else if event.active
            && event.renew
            && event.generation == press.generation
            && press.until.is_some()
        {
            press.until = Some(now + self.config.manual_lease);
        } else if event.active
            && !event.renew
            && event.generation > press.generation
            && press.until.is_none()
            && !press.needs_release
        {
            press.generation = event.generation;
            press.needs_release = !permitted;
            if permitted {
                press.until = Some(now + self.config.manual_lease);
                self.epoch = self.epoch.wrapping_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(mode: InputMode) -> InputGate {
        InputGate::new(GateConfig {
            mode,
            enabled: true,
            pause_while_speaking: true,
            manual_lease: Duration::from_secs(3),
            speaking_tail: Duration::from_millis(400),
        })
        .unwrap()
    }

    #[test]
    fn expiry_and_mute_require_release_and_renew_cannot_reopen() {
        let mut gate = gate(InputMode::PushToTalk);
        let now = Instant::now();
        let mut event = ManualCapture {
            session: Uuid::new_v4(),
            generation: 1,
            active: true,
            cancelled: false,
            renew: false,
        };
        gate.apply(GateCommand::Manual(event), now).unwrap();
        let initial = gate.snapshot(now);
        assert!(initial.allowed);
        let expired = now + Duration::from_secs(3);
        assert!(!gate.snapshot(expired).allowed);
        assert!(gate.snapshot(expired).needs_release);
        event.renew = true;
        gate.apply(GateCommand::Manual(event), expired).unwrap();
        assert!(!gate.snapshot(expired).allowed);
        event.renew = false;
        event.active = false;
        gate.apply(GateCommand::Manual(event), expired).unwrap();
        event.active = true;
        event.generation = 2;
        gate.apply(GateCommand::Manual(event), expired).unwrap();
        assert!(gate.snapshot(expired).allowed);
        gate.apply(GateCommand::SetEnabled(false), expired).unwrap();
        gate.apply(GateCommand::SetEnabled(true), expired).unwrap();
        assert!(!gate.snapshot(expired).allowed);
        assert!(gate.snapshot(expired).needs_release);
    }

    #[test]
    fn normal_release_preserves_epoch_and_speaking_windows_overlap() {
        let mut gate = gate(InputMode::Radio);
        let now = Instant::now();
        let source = Uuid::new_v4();
        let mut press = ManualCapture {
            session: source,
            generation: 1,
            active: true,
            cancelled: false,
            renew: false,
        };
        gate.apply(GateCommand::Manual(press), now).unwrap();
        let epoch = gate.snapshot(now).epoch;
        press.active = false;
        gate.apply(GateCommand::Manual(press), now).unwrap();
        assert_eq!(gate.snapshot(now).epoch, epoch);
        let other = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let lease = Duration::from_secs(1);
        gate.apply(
            GateCommand::SpeakingStarted {
                source,
                utterance: a,
                sequence: 1,
                lease,
            },
            now,
        )
        .unwrap();
        gate.apply(
            GateCommand::SpeakingStarted {
                source: other,
                utterance: b,
                sequence: 1,
                lease,
            },
            now,
        )
        .unwrap();
        gate.apply(GateCommand::SourceDisconnected(source), now)
            .unwrap();
        let after_tail = now + Duration::from_millis(500);
        assert!(!gate.snapshot(after_tail).permitted);
        gate.apply(
            GateCommand::SpeakingStarted {
                source,
                utterance: a,
                sequence: 1,
                lease,
            },
            after_tail,
        )
        .unwrap();
        assert!(gate.snapshot(now + Duration::from_millis(1400)).permitted);
    }
}

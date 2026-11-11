//! Presentation mapping for an actual native model process, without an independent state owner.

use crate::ai_runtime::native_model::{
    NativeBackend, NativeFailure, NativeModelSnapshot, NativePhase,
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeModelView {
    pub model_id: String,
    pub backend: NativeBackend,
    pub session_id: Uuid,
    pub phase: NativePhase,
    pub can_infer: bool,
    pub weights_resident: bool,
    pub last_failure: Option<NativeFailure>,
}

pub struct NativeModelPresenter;

impl NativeModelPresenter {
    pub fn project(snapshot: &NativeModelSnapshot) -> NativeModelView {
        NativeModelView {
            model_id: snapshot.model_id.clone(),
            backend: snapshot.backend,
            session_id: snapshot.session_id,
            phase: snapshot.phase,
            can_infer: snapshot.phase == NativePhase::Ready,
            weights_resident: matches!(
                snapshot.phase,
                NativePhase::Loaded | NativePhase::Warm | NativePhase::Ready
            ),
            last_failure: snapshot.last_failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_never_promotes_installed_or_warm_to_ready() {
        let id = Uuid::new_v4();
        for phase in [
            NativePhase::Started,
            NativePhase::Loaded,
            NativePhase::Warm,
            NativePhase::Failed,
            NativePhase::Stopped,
        ] {
            let snapshot = NativeModelSnapshot {
                model_id: "rvc".into(),
                backend: NativeBackend::Cpu,
                session_id: id,
                phase,
                last_failure: None,
            };
            let view = NativeModelPresenter::project(&snapshot);
            assert_eq!(view.session_id, id);
            assert!(!view.can_infer);
            assert_eq!(
                view.weights_resident,
                matches!(phase, NativePhase::Loaded | NativePhase::Warm)
            );
        }
        let snapshot = NativeModelSnapshot {
            model_id: "rvc".into(),
            backend: NativeBackend::Cpu,
            session_id: id,
            phase: NativePhase::Ready,
            last_failure: None,
        };
        let view = NativeModelPresenter::project(&snapshot);
        assert!(view.can_infer && view.weights_resident);
    }
}

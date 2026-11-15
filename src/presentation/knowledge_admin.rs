//! One application-owned GraphMaintenance review for GUI and MCP.
//! Plans never leave this owner; confirmation consumes the preview once.
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    api::GraphMaintenanceApi,
    character::CharacterId,
    knowledge::maintenance::{GraphMaintenanceMode, GraphMaintenancePreview},
    memory::MemoryId,
};

struct PendingGraph {
    request_id: Uuid,
    character: CharacterId,
    plan: GraphMaintenancePreview,
}

#[derive(Clone)]
pub struct GraphMaintenancePresenter {
    pending: Arc<Mutex<Option<PendingGraph>>>,
    stop: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct GraphReview {
    pub request_id: Uuid,
    pub character: CharacterId,
    pub relation_count: usize,
    pub relations: Vec<crate::memory::ProvenancedRelation>,
}

impl GraphMaintenancePresenter {
    pub fn new(stop: CancellationToken) -> Self {
        Self {
            pending: Arc::new(Mutex::new(None)),
            stop,
        }
    }

    pub fn reindex_available(&self) -> Result<bool, String> {
        GraphMaintenanceApi::reindex_available().map_err(|error| error.to_string())
    }

    pub async fn preview(
        &self,
        request_id: Uuid,
        character: CharacterId,
        memory_id: MemoryId,
        mode: GraphMaintenanceMode,
    ) -> Result<GraphReview, String> {
        let mut pending = self.pending.lock().await;
        *pending = None;
        if self.stop.is_cancelled() {
            return Err("application is stopping".into());
        }
        if mode == GraphMaintenanceMode::Reindex
            && !GraphMaintenanceApi::reindex_available().map_err(|e| e.to_string())?
        {
            return Err("Graph Reindex backend is unavailable".into());
        }
        let as_of_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let plan = GraphMaintenanceApi::preview(character, &[memory_id], as_of_unix_ms, mode)
            .await
            .map_err(|e| e.to_string())?;
        if self.stop.is_cancelled() {
            return Err("application is stopping".into());
        }
        let relations = plan.inspected_relations();
        let review = GraphReview {
            request_id,
            character,
            relation_count: relations.len(),
            relations,
        };
        *pending = Some(PendingGraph {
            request_id,
            character,
            plan,
        });
        Ok(review)
    }

    pub async fn apply(&self, request_id: Uuid, character: CharacterId) -> Result<(), String> {
        let plan = {
            let mut pending = self.pending.lock().await;
            match pending.as_ref() {
                Some(p) if p.request_id == request_id && p.character == character => {
                    pending.take().expect("matching preview exists").plan
                }
                _ => return Err("Graph preview is missing or belongs to another selection".into()),
            }
        };
        GraphMaintenanceApi::apply_confirmed(character, plan, true, &self.stop)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub async fn abort(&self, request_id: Uuid) -> bool {
        let mut pending = self.pending.lock().await;
        if pending.as_ref().is_some_and(|p| p.request_id == request_id) {
            *pending = None;
            true
        } else {
            false
        }
    }

    pub async fn reset(&self) {
        *self.pending.lock().await = None;
    }
}

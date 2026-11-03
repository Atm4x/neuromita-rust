use std::sync::Arc;

use tokio::sync::watch;

use crate::generation::{
    PresetCatalog, PresetCatalogError, PresetCatalogSnapshot, PresetId, PresetRecord,
};

pub struct PresetsPresenter {
    catalog: Arc<PresetCatalog>,
}

impl PresetsPresenter {
    pub(crate) fn new(catalog: Arc<PresetCatalog>) -> Self {
        Self { catalog }
    }

    pub fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.catalog.observe()
    }

    pub fn list(&self) -> Vec<(PresetId, PresetRecord)> {
        self.catalog.list()
    }

    pub fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        self.catalog.create(record)
    }

    pub fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        self.catalog.update(id, record)
    }

    pub fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        self.catalog.delete(id)
    }

    pub fn require(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        if self.catalog.contains(id) {
            Ok(())
        } else {
            Err(PresetCatalogError::NotFound(id))
        }
    }
}

use std::{collections::BTreeMap, sync::Arc};

use neuromita::generation::{
    EnvironmentPresetCredentials, JsonPresetRepository, PresetCatalog, PresetDefinition, PresetId,
    PresetRecord,
};

struct TestDir(std::path::PathBuf);

impl TestDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("preset-order-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(dir: &TestDir) -> PresetCatalog {
    PresetCatalog::open(
        BTreeMap::new(),
        Arc::new(JsonPresetRepository::new(dir.0.join("presets.json"))),
        Arc::new(EnvironmentPresetCredentials),
    )
    .unwrap()
}

fn record(name: &str) -> PresetRecord {
    PresetRecord {
        name: name.to_owned(),
        definition: PresetDefinition::Unconfigured,
    }
}

fn ids(catalog: &PresetCatalog) -> Vec<PresetId> {
    catalog.list().into_iter().map(|(id, _)| id).collect()
}

#[test]
fn order_is_persistent_without_changing_ids_or_records() {
    let dir = TestDir::new();
    let catalog = open(&dir);
    let a = catalog.create(record("first")).unwrap();
    let b = catalog.create(record("second")).unwrap();
    let c = catalog.create(record("third")).unwrap();
    let revision = catalog.observe().borrow().revision;
    catalog.reorder(&[c, a, b]).unwrap();
    assert_eq!(ids(&catalog), [c, a, b]);
    assert_eq!(catalog.observe().borrow().order, [c, a, b]);
    assert_eq!(catalog.observe().borrow().revision, revision + 1);
    catalog.reorder(&[c, a, b]).unwrap();
    assert_eq!(catalog.observe().borrow().revision, revision + 1);
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.0.join("presets.json")).unwrap()).unwrap();
    assert_eq!(json["order"], serde_json::json!([c.0, a.0, b.0]));
    assert_eq!(json["schema_version"], 1);
    drop(catalog);
    let reopened = open(&dir);
    assert_eq!(ids(&reopened), [c, a, b]);
    assert_eq!(reopened.list()[0].1.name, "third");
}

#[test]
fn invalid_reordering_is_rejected_without_changing_storage_or_revision() {
    let dir = TestDir::new();
    let catalog = open(&dir);
    let a = catalog.create(record("first")).unwrap();
    let b = catalog.create(record("second")).unwrap();
    let before = std::fs::read(dir.0.join("presets.json")).unwrap();
    let revision = catalog.observe().borrow().revision;
    for order in [vec![a], vec![a, a], vec![a, PresetId(999)], vec![b, a, a]] {
        assert!(catalog.reorder(&order).is_err());
        assert_eq!(ids(&catalog), [a, b]);
        assert_eq!(catalog.observe().borrow().revision, revision);
        assert_eq!(std::fs::read(dir.0.join("presets.json")).unwrap(), before);
    }
}

#[test]
fn legacy_catalog_without_order_uses_ids_until_first_ordered_write() {
    let dir = TestDir::new();
    let catalog = open(&dir);
    let a = catalog.create(record("first")).unwrap();
    let b = catalog.create(record("second")).unwrap();
    drop(catalog);
    let path = dir.0.join("presets.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("order");
    std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
    let reopened = open(&dir);
    assert_eq!(ids(&reopened), [a, b]);
    reopened.reorder(&[b, a]).unwrap();
    drop(reopened);
    assert_eq!(ids(&open(&dir)), [b, a]);
}

#[test]
fn create_and_delete_preserve_survivor_order_without_reusing_ids() {
    let dir = TestDir::new();
    let catalog = open(&dir);
    let a = catalog.create(record("first")).unwrap();
    let b = catalog.create(record("second")).unwrap();
    let c = catalog.create(record("third")).unwrap();
    catalog.reorder(&[c, a, b]).unwrap();
    catalog.delete(a).unwrap();
    assert_eq!(ids(&catalog), [c, b]);
    let d = catalog.create(record("fourth")).unwrap();
    assert!(d.0 > c.0);
    assert_eq!(ids(&catalog), [c, b, d]);
    drop(catalog);
    assert_eq!(ids(&open(&dir)), [c, b, d]);
}

#[test]
fn invalid_persisted_order_is_explicitly_rejected() {
    let dir = TestDir::new();
    let catalog = open(&dir);
    let a = catalog.create(record("first")).unwrap();
    let b = catalog.create(record("second")).unwrap();
    drop(catalog);
    let path = dir.0.join("presets.json");
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for invalid in [serde_json::json!([a.0, a.0]), serde_json::json!([b.0, 999])] {
        data["order"] = invalid;
        std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(
            PresetCatalog::open(
                BTreeMap::new(),
                Arc::new(JsonPresetRepository::new(path.clone())),
                Arc::new(EnvironmentPresetCredentials)
            )
            .is_err()
        );
    }
}

#[test]
fn single_step_moves_are_persisted_and_do_not_publish_on_boundaries() {
    let dir = TestDir::new();
    let catalog = open(&dir);
    let a = catalog.create(record("first")).unwrap();
    let b = catalog.create(record("second")).unwrap();
    let c = catalog.create(record("third")).unwrap();
    let revision = catalog.observe().borrow().revision;
    catalog.move_one(b, -1).unwrap();
    assert_eq!(ids(&catalog), [b, a, c]);
    assert_eq!(catalog.observe().borrow().revision, revision + 1);
    let bytes = std::fs::read(dir.0.join("presets.json")).unwrap();
    catalog.move_one(b, -1).unwrap();
    catalog.move_one(c, 1).unwrap();
    assert_eq!(catalog.observe().borrow().revision, revision + 1);
    assert_eq!(std::fs::read(dir.0.join("presets.json")).unwrap(), bytes);
    catalog.move_one(a, 1).unwrap();
    assert_eq!(ids(&catalog), [b, c, a]);
    assert_eq!(catalog.observe().borrow().revision, revision + 2);
    drop(catalog);
    assert_eq!(ids(&open(&dir)), [b, c, a]);
}

#[test]
fn rejected_single_step_moves_leave_catalog_and_disk_unchanged() {
    let dir = TestDir::new();
    let catalog = open(&dir);
    let first = catalog.create(record("first")).unwrap();
    let before = std::fs::read(dir.0.join("presets.json")).unwrap();
    let revision = catalog.observe().borrow().revision;
    for (id, step) in [(first, 0), (first, 2), (first, -7), (PresetId(999), 1)] {
        assert!(catalog.move_one(id, step).is_err());
    }
    assert_eq!(catalog.observe().borrow().revision, revision);
    assert_eq!(std::fs::read(dir.0.join("presets.json")).unwrap(), before);
}

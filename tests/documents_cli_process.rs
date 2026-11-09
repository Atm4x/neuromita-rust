use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use neuromita::{
    character::CharacterId,
    knowledge::document_index::{self, DocumentError, SearchStatus},
};
use serde_json::Value;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    documents: PathBuf,
    index: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("neuromita-document-r6-{}", Uuid::new_v4()));
        let documents = root.join("kind-documents");
        fs::create_dir_all(&documents).unwrap();
        fs::write(
            documents.join("notes.md"),
            "NeuroMita watches the garden of sakura trees.\nThe archive is in Moscow.",
        )
        .unwrap();
        fs::write(
            documents.join("details.txt"),
            "The garden has seven sakura trees.",
        )
        .unwrap();
        fs::write(documents.join("ignored.bin"), "not in scoped text corpus").unwrap();
        let index = root.join("documents-index.json");
        Self {
            root,
            documents,
            index,
        }
    }
    fn call(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_neuromita"))
            .arg("documents")
            .args(args)
            .env_remove("GEMINI_API_KEY")
            .output()
            .unwrap()
    }
    fn run(&self, mode: &str, extra: &[&str]) -> Output {
        let mut args = vec![
            mode,
            "--root",
            self.documents.to_str().unwrap(),
            "--character",
            "Kind",
            "--index",
            self.index.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        self.call(&args)
    }
    fn build(&self) {
        let r = self.run("build", &[]);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    }
    fn query(&self, text: &str) -> Output {
        self.run("query", &["--text", text])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn indexes_queries_and_returns_character_scoped_document_provenance() {
    let f = Fixture::new();
    f.build();
    let verify = f.run("verify", &[]);
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let output = f.query("sakura garden");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], "selected");
    assert_eq!(json["documents"], 2);
    assert_eq!(json["hits"].as_array().unwrap().len(), 2);
    assert_eq!(json["hits"][0]["source"], "Document");
    assert_eq!(json["hits"][0]["metadata"]["source_path"], "details.txt");
    assert_eq!(json["hits"][0]["metadata"]["chunk_index"], "0");
    assert_eq!(
        json["hits"][0]["metadata"]["source_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    let without_matches = f.query("elephant cobalt");
    assert!(without_matches.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&without_matches.stdout).unwrap()["status"],
        "no_matches"
    );
}

#[test]
fn changed_or_new_source_rejects_stale_index_until_explicit_rebuild_into_new_path() {
    let f = Fixture::new();
    f.build();
    fs::write(f.documents.join("details.txt"), "Completely changed text").unwrap();
    assert!(!f.query("garden").status.success());
    assert!(!f.run("verify", &[]).status.success());
    assert!(!f.run("build", &[]).status.success());
    let next = f.root.join("reindexed.json");
    let r = f.call(&[
        "build",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--index",
        next.to_str().unwrap(),
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    fs::write(f.documents.join("new.md"), "a new source appeared").unwrap();
    assert!(!f.query("garden").status.success());
}

#[test]
fn corrupt_or_modified_derived_index_is_rejected_without_source_write() {
    let f = Fixture::new();
    f.build();
    let before = fs::read(f.documents.join("notes.md")).unwrap();
    let mut index: Value = serde_json::from_slice(&fs::read(&f.index).unwrap()).unwrap();
    index["documents"][0]["chunks"][0] = "forged text".into();
    fs::write(&f.index, serde_json::to_vec_pretty(&index).unwrap()).unwrap();
    assert!(!f.query("forged").status.success());
    assert_eq!(fs::read(f.documents.join("notes.md")).unwrap(), before);
    fs::write(&f.index, "{ malformed").unwrap();
    assert!(!f.run("verify", &[]).status.success());
}

#[test]
fn cross_character_scope_and_limits_refuse_queries() {
    let f = Fixture::new();
    f.build();
    assert!(
        !f.call(&[
            "query",
            "--root",
            f.documents.to_str().unwrap(),
            "--character",
            "Crazy",
            "--index",
            f.index.to_str().unwrap(),
            "--text",
            "garden"
        ])
        .status
        .success()
    );
    assert!(
        !f.run("query", &["--text", "garden", "--limit", "21"])
            .status
            .success()
    );
    assert!(
        !f.run("query", &["--text", "x", "--limit", "5"])
            .status
            .success()
    );
    assert!(
        !f.run("query", &["--text", "garden", "--limit", "0"])
            .status
            .success()
    );
}

#[test]
fn empty_corpus_and_explicit_scope_are_honest() {
    let f = Fixture::new();
    fs::remove_file(f.documents.join("details.txt")).unwrap();
    fs::remove_file(f.documents.join("notes.md")).unwrap();
    f.build();
    let result = f.query("garden");
    assert!(result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["status"],
        "empty_corpus"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["documents"],
        0
    );
    assert!(!f.run("build", &[]).status.success());
}

#[cfg(unix)]
#[test]
fn symlinked_documents_and_indexes_refuse_without_clobbering() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.build();
    symlink(f.documents.join("notes.md"), f.documents.join("linked.md")).unwrap();
    assert!(!f.run("verify", &[]).status.success());
    assert!(!f.query("garden").status.success());
    assert!(!f.run("build", &[]).status.success());
    fs::remove_file(f.documents.join("linked.md")).unwrap();
    let other = f.root.join("linked-index.json");
    symlink(&f.index, &other).unwrap();
    let r = f.call(&[
        "query",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--index",
        other.to_str().unwrap(),
        "--text",
        "garden",
    ]);
    assert!(!r.status.success());
}

#[test]
fn retrieval_uses_existing_fusion_and_rejects_stale_index_in_core_api() {
    let f = Fixture::new();
    f.build();
    let result =
        document_index::search_verified(&f.documents, CharacterId::Kind, &f.index, "garden", 1)
            .unwrap();
    assert_eq!(result.status, SearchStatus::Selected);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(
        result.hits[0].key.source,
        neuromita::knowledge::retrieval_fusion::RetrievalSource::Document
    );
    assert_eq!(result.hits[0].key.character, CharacterId::Kind);
    assert!(matches!(
        document_index::search_verified(&f.documents, CharacterId::Crazy, &f.index, "garden", 1),
        Err(DocumentError::WrongScope)
    ));
}

#[test]
fn invalid_utf8_and_oversized_text_are_not_silently_indexed() {
    let f = Fixture::new();
    fs::write(f.documents.join("broken.md"), [0xff, 0xfe, 0xfd]).unwrap();
    assert!(!f.run("build", &[]).status.success());
    fs::remove_file(f.documents.join("broken.md")).unwrap();
    fs::write(f.documents.join("huge.txt"), vec![b'a'; 128 * 1024 + 1]).unwrap();
    assert!(!f.run("build", &[]).status.success());
    assert!(!f.index.exists());
}

#[test]
fn unicode_documents_are_chunked_deterministically_and_keep_stable_keys() {
    let f = Fixture::new();
    fs::write(
        f.documents.join("notes.md"),
        format!("{} сакура эндшпиль", "земля ".repeat(170)),
    )
    .unwrap();
    f.build();
    let actual = document_index::load_verified(&f.documents, CharacterId::Kind, &f.index).unwrap();
    let notes = actual
        .documents
        .iter()
        .find(|d| d.path == "notes.md")
        .unwrap();
    assert!(notes.chunks.len() >= 2);
    assert!(notes.chunks.iter().all(|s| s.chars().count() <= 800));
    let first = f.query("сакура эндшпиль");
    let second = f.query("сакура эндшпиль");
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    let hits: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert!(
        hits["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == "notes.md#1")
    );
}

#[test]
fn competing_builds_never_overwrite_a_published_index() {
    let f = Fixture::new();
    let arguments = [
        "build",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--index",
        f.index.to_str().unwrap(),
    ];
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| f.call(&arguments));
        let b = scope.spawn(|| f.call(&arguments));
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(a.status.success(), b.status.success());
    assert!(f.run("verify", &[]).status.success());
}

#[test]
fn reindex_prepares_immutable_distinct_revisions_without_activation() {
    let f = Fixture::new();
    let revisions = f.root.join("revisions");
    fs::create_dir(&revisions).unwrap();
    let prepare = || {
        let result = f.call(&[
            "reindex",
            "--root",
            f.documents.to_str().unwrap(),
            "--character",
            "Kind",
            "--revisions-dir",
            revisions.to_str().unwrap(),
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice::<Value>(&result.stdout).unwrap()
    };
    let first = prepare();
    let original = PathBuf::from(first["index"].as_str().unwrap());
    let previous_bytes = fs::read(&original).unwrap();
    let second = prepare();
    let subsequent = PathBuf::from(second["index"].as_str().unwrap());
    assert_ne!(original, subsequent);
    assert_ne!(first["index"], f.index.to_string_lossy().as_ref());
    assert_eq!(first["fingerprint"], second["fingerprint"]);
    assert_eq!(fs::read(&original).unwrap(), previous_bytes);
    assert_eq!(fs::read_dir(&revisions).unwrap().count(), 2);

    fs::write(
        f.documents.join("notes.md"),
        "Brand new violet orchard revision.",
    )
    .unwrap();
    let third = prepare();
    assert_ne!(third["fingerprint"], first["fingerprint"]);
    assert_eq!(fs::read(&original).unwrap(), previous_bytes);
    let old = f.call(&[
        "verify",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--index",
        original.to_str().unwrap(),
    ]);
    assert!(!old.status.success());
    let new = f.call(&[
        "verify",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--index",
        third["index"].as_str().unwrap(),
    ]);
    assert!(
        new.status.success(),
        "{}",
        String::from_utf8_lossy(&new.stderr)
    );
}

#[test]
fn reindex_rejects_revision_directory_inside_documents_and_invalid_character() {
    let f = Fixture::new();
    let inside = f.documents.join("index-revisions");
    fs::create_dir(&inside).unwrap();
    let invalid = f.call(&[
        "reindex",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--revisions-dir",
        inside.to_str().unwrap(),
    ]);
    assert!(!invalid.status.success());
    assert_eq!(fs::read_dir(&inside).unwrap().count(), 0);
    let wrong = f.call(&[
        "reindex",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Unknown",
        "--revisions-dir",
        f.root.to_str().unwrap(),
    ]);
    assert!(!wrong.status.success());
}

#[test]
fn explicit_import_updates_canonical_corpus_but_never_activates_stale_index() {
    let f = Fixture::new();
    f.build();
    let source = f.root.join("external.txt");
    fs::write(&source, "The migrating cranes return in autumn.").unwrap();
    let output = f.call(&[
        "import",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--source",
        source.to_str().unwrap(),
        "--name",
        "season.md",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["character"], "Kind");
    assert_eq!(receipt["name"], "season.md");
    assert_eq!(receipt["requires_reindex"], true);
    assert_eq!(receipt["source_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(
        fs::read(f.documents.join("season.md")).unwrap(),
        fs::read(&source).unwrap()
    );
    assert!(!f.run("verify", &[]).status.success());
    assert!(!f.query("cranes").status.success());
    let revisions = f.root.join("revisions");
    fs::create_dir(&revisions).unwrap();
    let next = f.call(&[
        "reindex",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--revisions-dir",
        revisions.to_str().unwrap(),
    ]);
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    let next_receipt: Value = serde_json::from_slice(&next.stdout).unwrap();
    let new_index = next_receipt["index"].as_str().unwrap();
    let query = f.call(&[
        "query",
        "--root",
        f.documents.to_str().unwrap(),
        "--character",
        "Kind",
        "--index",
        new_index,
        "--text",
        "migrating cranes",
    ]);
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&query.stdout).unwrap()["status"],
        "selected"
    );
}

#[test]
fn import_rejects_unsafe_and_oversized_source_without_mutating_corpus() {
    let f = Fixture::new();
    let original = fs::read(f.documents.join("notes.md")).unwrap();
    let source = f.root.join("incoming.md");
    fs::write(&source, "valid text content").unwrap();
    for name in [
        "../escape.md",
        ".private.md",
        "bad.txt/another",
        "bad.pdf",
        " notes.md",
        "notes.md",
    ] {
        let output = f.call(&[
            "import",
            "--root",
            f.documents.to_str().unwrap(),
            "--character",
            "Kind",
            "--source",
            source.to_str().unwrap(),
            "--name",
            name,
        ]);
        assert!(!output.status.success(), "{name:?}");
    }
    assert_eq!(fs::read(f.documents.join("notes.md")).unwrap(), original);
    assert!(!f.documents.join("escape.md").exists());
    fs::write(&source, vec![b'a'; 128 * 1024 + 1]).unwrap();
    assert!(
        !f.call(&[
            "import",
            "--root",
            f.documents.to_str().unwrap(),
            "--character",
            "Kind",
            "--source",
            source.to_str().unwrap(),
            "--name",
            "large.txt"
        ])
        .status
        .success()
    );
    fs::write(&source, [0xff, 0xfe, 0x00]).unwrap();
    assert!(
        !f.call(&[
            "import",
            "--root",
            f.documents.to_str().unwrap(),
            "--character",
            "Kind",
            "--source",
            source.to_str().unwrap(),
            "--name",
            "invalid.txt"
        ])
        .status
        .success()
    );
    assert!(fs::read_dir(&f.documents).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".neuromita-document-import-")
    }));
    assert!(!f.documents.join("large.txt").exists());
    assert!(!f.documents.join("invalid.txt").exists());
}

#[test]
fn racing_importers_never_replace_canonical_document() {
    let f = Fixture::new();
    let a = f.root.join("a.txt");
    let b = f.root.join("b.txt");
    fs::write(&a, "first canonical choice").unwrap();
    fs::write(&b, "second canonical choice").unwrap();
    let exe = env!("CARGO_BIN_EXE_neuromita").to_string();
    let root = f.documents.to_string_lossy().into_owned();
    let a = a.to_string_lossy().into_owned();
    let b = b.to_string_lossy().into_owned();
    let handles = [a, b].map(|source| {
        let root = root.clone();
        let exe = exe.clone();
        std::thread::spawn(move || {
            Command::new(exe)
                .args([
                    "documents",
                    "import",
                    "--root",
                    &root,
                    "--character",
                    "Kind",
                    "--source",
                    &source,
                    "--name",
                    "race.md",
                ])
                .output()
                .unwrap()
        })
    });
    let output = handles.map(|handle| handle.join().unwrap());
    assert_eq!(output.iter().filter(|o| o.status.success()).count(), 1);
    let actual = fs::read_to_string(f.documents.join("race.md")).unwrap();
    assert!(actual == "first canonical choice" || actual == "second canonical choice");
    assert!(fs::read_dir(&f.documents).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".neuromita-document-import-")
    }));
}

#[cfg(unix)]
#[test]
fn import_rejects_symlink_inputs_and_existing_symlink_targets() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let source = f.root.join("original.md");
    let link = f.root.join("linked.md");
    fs::write(&source, "source owner test").unwrap();
    symlink(&source, &link).unwrap();
    let call = |input: &str, name: &str| {
        f.call(&[
            "import",
            "--root",
            f.documents.to_str().unwrap(),
            "--character",
            "Kind",
            "--source",
            input,
            "--name",
            name,
        ])
    };
    assert!(!call(link.to_str().unwrap(), "linked.md").status.success());
    let target = f.documents.join("already.md");
    symlink(&source, &target).unwrap();
    assert!(
        !call(source.to_str().unwrap(), "already.md")
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), "source owner test");
}

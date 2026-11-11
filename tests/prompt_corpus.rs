use std::{collections::BTreeMap, path::PathBuf};

use neuromita::{character::CharacterId, prompts::PromptCatalog};
use ring::digest::{SHA256, digest};
use serde_json::Value;

fn fingerprint(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n");
    let normalized = normalized.trim_end();
    digest(&SHA256, normalized.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn supplied_prompt_corpus_renders_and_matches_python_reference() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prompt-corpus");
    let root = directory.join("Prompts");
    let reference: Value = serde_json::from_slice(
        &std::fs::read(directory.join("python_reference_sha256.json")).unwrap(),
    )
    .unwrap();
    let expected = reference["sets"].as_object().unwrap();
    let differences = reference["known_python_semantics_differences"]
        .as_object()
        .unwrap();
    let catalog = PromptCatalog::new(root);
    let mut total = 0;
    let mut matches = 0;
    for character in CharacterId::ALL {
        for info in catalog.list(character).unwrap() {
            let name = format!("{character}/{}", info.name);
            let expected = &expected[&name];
            assert!(!expected.is_null(), "unregistered prompt set {name}");
            let snapshot = catalog
                .capture(character, Some(&info.name))
                .unwrap()
                .unwrap();
            let rendered = snapshot
                .render(BTreeMap::new(), "main_template.txt")
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                !rendered.stable.is_empty(),
                "{name} rendered no stable instructions"
            );
            let stable = rendered
                .stable
                .iter()
                .map(|s| fingerprint(s))
                .collect::<Vec<_>>();
            let context = rendered
                .context
                .iter()
                .map(|s| fingerprint(s))
                .collect::<Vec<_>>();
            let python_stable: Vec<String> =
                serde_json::from_value(expected["stable"].clone()).unwrap();
            let python_context: Vec<String> =
                serde_json::from_value(expected["context"].clone()).unwrap();
            assert_eq!(
                stable.len(),
                python_stable.len(),
                "{name}: stable segment count"
            );
            assert_eq!(
                context.len(),
                python_context.len(),
                "{name}: context segment count"
            );
            if !differences.contains_key(&name) {
                assert_eq!(stable, python_stable, "{name}: stable segment content");
                assert_eq!(context, python_context, "{name}: context segment content");
                matches += 1;
            }
            total += 1;
        }
    }
    assert_eq!(total, 31, "uploaded corpus set coverage changed");
    assert_eq!(matches, 21, "Python/Rust exact-match coverage changed");
}

#[test]
fn supplied_prompt_corpus_preserves_embedded_json_and_local_scope() {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prompt-corpus/Prompts");
    let catalog = PromptCatalog::new(root);
    let crazy = catalog
        .capture(CharacterId::Crazy, Some("Default"))
        .unwrap()
        .unwrap()
        .render(BTreeMap::new(), "main_template.txt")
        .unwrap();
    let text = crazy.stable.join("\n");
    assert!(text.contains(r#""intents": [{"type": "intent.type", "payload": {}}]"#));
    assert!(
        crazy
            .context
            .iter()
            .any(|text| text.contains("Love: 1/100"))
    );
    let json_set = catalog
        .capture(CharacterId::Crazy, Some("DefaultJson"))
        .unwrap()
        .unwrap()
        .render(BTreeMap::new(), "main_template.txt")
        .unwrap();
    assert!(
        json_set
            .stable
            .iter()
            .any(|text| text.starts_with("Current context:"))
    );
}

#[test]
fn supplied_prompt_corpus_all_seed_paths_are_interpretable() {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prompt-corpus/Prompts");
    let catalog = PromptCatalog::new(root);
    let flags = [
        "SEED_INITIAL_MEMORIES",
        "SEED_CHARACTER_MEMORIES",
        "SEED_WORLD_LORE",
        "SEED_ROOMS_LORE",
        "SEED_MITAS_LORE",
        "SEED_ENTITY_LINKS",
    ];
    for character in CharacterId::ALL {
        for info in catalog.list(character).unwrap() {
            let variables = flags
                .iter()
                .map(|flag| ((*flag).to_owned(), Value::Bool(true)))
                .collect();
            let snapshot = catalog
                .capture(character, Some(&info.name))
                .unwrap()
                .unwrap();
            snapshot
                .render(variables, "main_template.txt")
                .unwrap_or_else(|error| panic!("{character}/{} seed: {error}", info.name));
        }
    }
}

#[test]
fn supplied_postdsl_rules_match_python_reference() {
    use neuromita::{generation::StructuredResponse, prompts::PostDslProgram};
    use serde_json::{Map, json};

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prompt-corpus");
    let reference: Value =
        serde_json::from_slice(&std::fs::read(root.join("python_postdsl_reference.json")).unwrap())
            .unwrap();
    let sample = reference["text_input"].as_str().unwrap();
    let mut total = 0;
    for (name, expected) in reference["sets"].as_object().unwrap() {
        let script = std::fs::read_to_string(
            root.join("Prompts")
                .join(name)
                .join("PostScripts/main_rules.postscript"),
        )
        .unwrap();
        let program = PostDslProgram::parse(&script, BTreeMap::new()).unwrap();
        let mut vars = BTreeMap::from([
            ("Love".into(), json!(5)),
            ("attitude".into(), json!(60)),
            ("boredom".into(), json!(20)),
            ("stress".into(), json!(10)),
            ("secretExposed".into(), json!(false)),
        ]);
        let mut response = StructuredResponse::spoken(sample);
        if name == "Crazy/Default" || name == "Crazy/DefaultJson" {
            response.custom_fields = Some(Map::from_iter([
                ("love_change".into(), json!(2.0)),
                ("secret_trial".into(), json!(true)),
            ]));
        }
        let result = program
            .apply(response, &vars)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        vars.extend(result.variables);
        assert_eq!(
            fingerprint(&result.response.segments[0].text),
            expected["text_sha256"],
            "{name}: Python PostDSL text mismatch"
        );
        assert_eq!(
            serde_json::to_value(vars).unwrap(),
            expected["variables"],
            "{name}: Python PostDSL variable mismatch"
        );
        total += 1;
    }
    assert_eq!(total, 9);
}

#[test]
fn supplied_custom_params_apply_real_formulas_and_boolean_fields() {
    use neuromita::generation::StructuredResponse;
    use serde_json::{Map, json};

    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prompt-corpus/Prompts");
    let catalog = PromptCatalog::new(root);
    for name in ["By_mactep_kot_new_mini", "By_mactep_kot_new_mini_Json"] {
        let snapshot = catalog
            .capture(CharacterId::Crazy, Some(name))
            .unwrap()
            .unwrap();
        let program = snapshot.post_dsl().unwrap();
        let mut response = StructuredResponse::spoken("Hello");
        response.custom_fields = Some(Map::from_iter([
            ("love_change".into(), json!(2.0)),
            ("secret_trial".into(), json!(true)),
        ]));
        let vars = BTreeMap::from([("Love".into(), json!(5))]);
        let result = program.apply(response, &vars).unwrap();
        assert_eq!(result.variables["Love"], json!(7.0));
        assert_eq!(result.variables["secret_trial"], json!(true));
    }
}

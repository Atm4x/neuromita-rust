#[path = "localization/extract.rs"]
mod extract;

fn main() {
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=localization");
    println!("cargo:rerun-if-changed=src/character/model.rs");
    let messages = extract::extract_sources(&root).expect("extract localization declarations");
    std::fs::write(
        output.join("localization-messages.json"),
        serde_json::to_string_pretty(&messages).unwrap(),
    )
    .unwrap();
    let mut bundled = String::new();
    for (constant, folder) in [
        ("BUNDLED_LEGACY", "localization/python"),
        ("BUNDLED_OVERRIDES", "localization/catalogs"),
    ] {
        bundled.push_str(&format!("const {constant}: &[(&str, &str)] = &[\n"));
        let mut paths = std::fs::read_dir(root.join(folder))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let name = path.file_stem().unwrap().to_str().unwrap();
            if path.extension().is_none_or(|e| e != "json")
                || name.starts_with('_')
                || name == "manifest"
            {
                continue;
            }
            bundled.push_str(&format!(
                "({:?}, include_str!({:?})),\n",
                name,
                path.to_str().unwrap()
            ));
        }
        bundled.push_str("];\n");
    }
    std::fs::write(output.join("localization-bundled.rs"), bundled).unwrap();
    let mut speech_manifests = String::from("const BUNDLED_SPEECH_MANIFESTS: &[&str] = &[\n");
    let builtin_plugins = root.join("python/plugins/builtin");
    if let Ok(entries) = std::fs::read_dir(&builtin_plugins) {
        let mut manifests = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("plugin.json"))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        manifests.sort();
        for path in manifests {
            println!("cargo:rerun-if-changed={}", path.display());
            speech_manifests.push_str(&format!(
                "    include_str!({:?}),\n",
                path.to_str().unwrap()
            ));
        }
    }
    speech_manifests.push_str("];\n");
    std::fs::write(output.join("speech-plugin-manifests.rs"), speech_manifests).unwrap();
    #[cfg(feature = "gui")]
    bundle_prompts(&root.join("Prompts"), &output);
    #[cfg(feature = "gui")]
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles),
    )
    .expect("compile desktop interface");
}

#[cfg(feature = "gui")]
fn bundle_prompts(root: &std::path::Path, output: &std::path::Path) {
    use std::fmt::Write;

    fn collect(directory: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("read bundled prompts directory") {
            let entry = entry.expect("read bundled prompt entry");
            let kind = entry.file_type().expect("read bundled prompt entry type");
            if kind.is_dir() {
                collect(&entry.path(), files);
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }

    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();
    assert!(!files.is_empty(), "bundled prompt directory is empty");
    let mut source = String::from("const BUNDLED_PROMPTS: &[(&str, &[u8])] = &[\n");
    for path in files {
        let relative = path.strip_prefix(root).unwrap();
        writeln!(source, "({relative:?}, include_bytes!({path:?})),").unwrap();
    }
    source.push_str("];\n");
    std::fs::write(output.join("bundled-prompts.rs"), source).expect("write bundled prompts");
}

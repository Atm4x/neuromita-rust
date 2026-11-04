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
    #[cfg(feature = "gui")]
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles),
    )
    .expect("compile desktop interface");
}

use std::{fs, io, path::Path};

use super::PromptCatalog;

include!(concat!(env!("OUT_DIR"), "/bundled-prompts.rs"));

impl PromptCatalog {
    pub fn open_portable(data_directory: &Path) -> io::Result<Self> {
        let root = data_directory.join("Prompts");
        match fs::metadata(&root) {
            Ok(directory) if directory.is_dir() => return Ok(Self::new(root)),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("prompt catalog path is not a directory: {}", root.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let staging = data_directory.join(format!(".prompts-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&staging)?;
        let result = (|| {
            for (name, bytes) in BUNDLED_PROMPTS {
                let file = staging.join(name);
                fs::create_dir_all(file.parent().expect("bundled prompt has a parent"))?;
                fs::write(file, bytes)?;
            }
            match fs::rename(&staging, &root) {
                Err(_) if root.is_dir() => Ok(()),
                result => result,
            }
        })();
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        result?;
        Ok(Self::new(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_bundle_is_seeded_once_and_preserves_local_edits() {
        let profile =
            std::env::temp_dir().join(format!("neuromita-prompts-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&profile).unwrap();
        let catalog = PromptCatalog::open_portable(&profile).unwrap();
        assert_eq!(catalog.root(), profile.join("Prompts"));
        let relative = Path::new("Cappie/DefaultJson/react_template.txt");
        let reaction = catalog.root().join(relative);
        assert_eq!(
            fs::read(&reaction).unwrap(),
            include_bytes!("../../Prompts/Cappie/DefaultJson/react_template.txt")
        );
        fs::write(&reaction, "local prompt").unwrap();
        let reopened = PromptCatalog::open_portable(&profile).unwrap();
        assert_eq!(
            fs::read_to_string(reopened.root().join(relative)).unwrap(),
            "local prompt"
        );
        assert_eq!(fs::read_dir(&profile).unwrap().count(), 1);
        fs::remove_dir_all(profile).unwrap();
    }
}

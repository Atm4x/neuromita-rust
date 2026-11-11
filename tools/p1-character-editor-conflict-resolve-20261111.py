from pathlib import Path
import re

root = Path(".")
pattern = re.compile(r"^<<<<<<< ours\n(?P<ours>.*?)^=======\n(?P<theirs>.*?)^>>>>>>> theirs\n", re.M | re.S)

def resolve(file, transform, expected):
    path = root / file
    source = path.read_text()
    count = 0
    def replace(match):
        nonlocal count
        count += 1
        return transform(count, match.group("ours"), match.group("theirs"))
    resolved = pattern.sub(replace, source)
    if count != expected or "<<<<<<<" in resolved:
        raise RuntimeError(f"{file}: expected {expected} conflicts, got {count}")
    path.write_text(resolved)

resolve("src/bin/neuromita-gui/runtime.rs", lambda n, o, t: o + t, 1)
resolve("ui/app.slint", lambda n, o, t: o.replace("connecting: root.core-connecting;", "connecting: root.core-connecting; active-character: root.chat-character;"), 1)
resolve("ui/pages/settings.slint", lambda n, o, t: "" if n == 1 else o.replace("connected: root.connected;", "connected: root.connected; active-character: root.active-character;"), 2)
page = root / "ui/pages/settings.slint"
content = page.read_text()
needle = "SettingsUi.character-load(entry.index);"
if content.count(needle) != 1:
    raise RuntimeError("unexpected settings selection code")
page.write_text(content.replace(needle, needle + "\n                                SettingsUi.character-debug-select(entry.index);"))
resolve("src/bin/neuromita-gui/binding_tests.rs", lambda n, o, t: o + t, 1)

def sandbox(n, ours, theirs):
    if n != 1 or "Command::SaveCharacterProfile" not in theirs:
        raise RuntimeError("unexpected dispatch shape")
    return ours + "                }\n" + theirs[theirs.index("                Command::SaveCharacterProfile"):]

resolve("src/bin/neuromita-gui/sandbox.rs", sandbox, 1)

def settings(n, ours, theirs):
    if n == 1:
        return "        CharacterSettings, OperationPresets, PersistenceState, PresetSelection, SettingsArea,\n        SettingsSnapshot, StructuredDisplayMode,\n"
    if n == 2:
        return ours + "            }\n        }\n    });\n    let send = dispatch.clone();\n    let weak = window.as_weak();\n" + theirs
    if n == 3:
        return ours + "            }\n        }\n    });\n    let send = dispatch.clone();\n" + theirs
    raise RuntimeError("unexpected Settings presenter conflict")

resolve("src/bin/neuromita-gui/settings.rs", settings, 3)
path = root / "src/bin/neuromita-gui/settings.rs"
content = path.read_text()
before = "                    .set_settings_error(error.into());\n            }\n            }\n        }\n    });"
after = "                    .set_settings_error(error.into());\n            }\n        }\n    });"
if content.count(before) != 1:
    raise RuntimeError("unexpected dedup handler closure")
path.write_text(content.replace(before, after))

gui = root / "src/bin/neuromita-gui/settings.rs"
content = gui.read_text()
previous = "api.settings\\n                .request_save()\\n                .map_err(|error| error.to_string())?;"
updated = "api.settings\\n                .retry_failed_save()\\n                .map_err(|error| error.to_string())?;"
if content.count(previous) != 1:
    raise RuntimeError("old editor retry call path changed")
gui.write_text(content.replace(previous, updated))
presenter = root / "src/presentation/settings.rs"
content = presenter.read_text()
anchor = "    pub fn snapshot(&self) -> Result<Arc<SettingsSnapshot>, SettingsError> {\\n        self.owner.snapshot_all()\\n    }\\n"
method = """    pub fn retry_failed_save(&self) -> Result<(), SettingsError> {
        let snapshot = self.owner.snapshot_all()?;
        if !matches!(snapshot.persistence, PersistenceState::Failed { .. }) {
            return Err(SettingsError::Validation(
                "there is no failed settings write to retry".into(),
            ));
        }
        self.owner.request_save()
    }

"""
if content.count(anchor) != 1:
    raise RuntimeError("settings presenter method insertion site changed")
presenter.write_text(content.replace(anchor, method + anchor))

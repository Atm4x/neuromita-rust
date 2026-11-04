//! Best-effort read-only input discovery; this does not open a microphone.

use std::{path::Path, process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInputDevice {
    pub id: String,
    pub name: String,
    pub backend: String,
}

pub struct AudioInputCatalog;

impl AudioInputCatalog {
    pub async fn discover(ffmpeg: Option<&Path>) -> Vec<AudioInputDevice> {
        #[cfg(target_os = "linux")]
        {
            let output = Self::run("pactl", &["list", "short", "sources"]).await;
            if let Some(output) = output {
                return parse_pulse_sources(&output);
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(exe) = ffmpeg.filter(|path| path.is_absolute() && path.is_file())
            && let Some(output) = Self::run(
                exe.to_string_lossy().as_ref(),
                &[
                    "-hide_banner",
                    "-list_devices",
                    "true",
                    "-f",
                    "dshow",
                    "-i",
                    "dummy",
                ],
            )
            .await
        {
            return parse_dshow_devices(&output);
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let _ = ffmpeg;
        #[cfg(target_os = "linux")]
        let _ = ffmpeg;
        Vec::new()
    }

    async fn run(binary: &str, args: &[&str]) -> Option<String> {
        let child = Command::new(binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let output = timeout(Duration::from_secs(3), child.wait_with_output())
            .await
            .ok()?
            .ok()?;
        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (text.len() <= 512 * 1024).then_some(text)
    }
}

#[cfg(target_os = "linux")]
fn parse_pulse_sources(text: &str) -> Vec<AudioInputDevice> {
    text.lines()
        .take(256)
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            let id = fields.get(1)?.trim();
            if id.is_empty() || id.contains(".monitor") || id.len() > 512 {
                return None;
            }
            Some(AudioInputDevice {
                id: id.into(),
                name: id.into(),
                backend: "pulse".into(),
            })
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn parse_dshow_devices(text: &str) -> Vec<AudioInputDevice> {
    let mut legacy_audio = false;
    let mut pending = None;
    let mut devices = Vec::new();
    for line in text.lines().take(512) {
        if line.contains("DirectShow audio devices") {
            legacy_audio = true;
            pending = None;
        } else if line.contains("DirectShow video devices") {
            legacy_audio = false;
            pending = None;
        }
        let Some((_, message)) = line.split_once(']') else {
            continue;
        };
        let message = message.trim();
        if let Some(alternative) = message.strip_prefix("Alternative name ")
            && let Some(name) = pending.take()
            && let Some(identity) = alternative
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
            && identity.starts_with("@device_")
            && identity.len() <= 506
        {
            let id = format!("audio={identity}");
            if !devices
                .iter()
                .any(|device: &AudioInputDevice| device.id == id)
            {
                devices.push(AudioInputDevice {
                    id,
                    name,
                    backend: "dshow".into(),
                });
            }
        } else if let Some((name, suffix)) =
            message.strip_prefix('"').and_then(|s| s.split_once('"'))
        {
            pending = (name.len() <= 512
                && !name.is_empty()
                && (suffix.trim() == "(audio)" || (suffix.trim().is_empty() && legacy_audio)))
                .then(|| name.to_owned());
        } else {
            pending = None;
        }
    }
    devices
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    #[test]
    fn dshow_uses_alternative_identity_for_same_name_and_ignores_video() {
        let text = r#"[in#0 @ 123] "Camera" (video)
[in#0 @ 123] Alternative name "@device_pnp_cam"
[in#0 @ 123] "USB microphone" (audio)
[in#0 @ 123] Alternative name "@device_cm_first"
[in#0 @ 123] "USB microphone" (audio)
[in#0 @ 123] Alternative name "@device_cm_second"
[in#0 @ 123] "missing identifier" (audio)"#;
        let ids: Vec<_> = parse_dshow_devices(text)
            .into_iter()
            .map(|device| device.id)
            .collect();
        assert_eq!(ids, ["audio=@device_cm_first", "audio=@device_cm_second"]);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_excludes_loopback_monitors() {
        let result = parse_pulse_sources(
            "0\talsa_input.usb\tmodule-alsa-card.c\n1\talsa_output.monitor\tmodule-alsa-card.c",
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "alsa_input.usb");
    }
}

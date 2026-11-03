//! Read-only desktop presentation of detected acceleration and AI runtime state.
//! Worker control and environment maintenance remain owned by the AI runtime.

use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiHardwareDevice {
    pub name: String,
    pub vendor: String,
    pub cuda: String,
    pub directml: String,
    pub capability: String,
    pub vram: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AiHardwareSnapshot {
    pub devices: Vec<AiHardwareDevice>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiEngineSettingsSnapshot {
    pub process_mode: String,
    pub process_status: String,
    pub environment_path: Option<String>,
    pub maintenance_status: String,
    pub process_control_available: bool,
    pub maintenance_available: bool,
    pub hub_available: bool,
}

#[derive(Default)]
pub struct AiEngineSettingsPresenter;

impl AiEngineSettingsPresenter {
    pub fn new() -> Self {
        Self
    }

    pub fn snapshot(&self) -> AiEngineSettingsSnapshot {
        let configured_mode = std::env::var("NEUROMITA_AI_ENGINE_MODE")
            .ok()
            .filter(|mode| matches!(mode.as_str(), "shared" | "split"))
            .unwrap_or_else(|| "shared".into());
        AiEngineSettingsSnapshot {
            process_mode: configured_mode,
            process_status: "Состояние процессов недоступно".into(),
            environment_path: None,
            maintenance_status: "Сервис обслуживания не подключён".into(),
            process_control_available: false,
            maintenance_available: false,
            hub_available: false,
        }
    }

    pub fn hardware(&self) -> AiHardwareSnapshot {
        let query = Command::new("nvidia-smi")
            .args([
                "--query-gpu=name,memory.total,compute_cap",
                "--format=csv,noheader,nounits",
            ])
            .output();
        if let Ok(output) = query
            && output.status.success()
        {
            let devices = parse_nvidia_smi(&String::from_utf8_lossy(&output.stdout));
            if !devices.is_empty() {
                return AiHardwareSnapshot {
                    devices,
                    error: None,
                };
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(devices) = windows_adapters() {
            if !devices.is_empty() {
                return AiHardwareSnapshot {
                    devices,
                    error: None,
                };
            }
        }
        AiHardwareSnapshot {
            devices: Vec::new(),
            error: Some("Аппаратный профиль недоступен".into()),
        }
    }
}

fn parse_nvidia_smi(csv: &str) -> Vec<AiHardwareDevice> {
    csv.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let columns = line.split(',').map(str::trim).collect::<Vec<_>>();
            let name = columns.first().copied().unwrap_or_default();
            if columns.len() != 3 || name.is_empty() {
                return None;
            }
            let capability = columns[2];
            Some(AiHardwareDevice {
                name: name.to_string(),
                vendor: "NVIDIA".into(),
                cuda: format!("cuda:{index}"),
                directml: String::new(),
                capability: if capability == "N/A" || capability.is_empty() {
                    String::new()
                } else {
                    format!("SM_{capability}")
                },
                vram: columns[1]
                    .parse::<u64>()
                    .ok()
                    .map(|mib| format!("{:.1} GB", mib as f64 / 1024.0))
                    .unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn windows_adapters() -> Option<Vec<AiHardwareDevice>> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_VideoController | Select-Object Name,AdapterCompatibility | ConvertTo-Json -Compress",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let rows = match json {
        serde_json::Value::Array(rows) => rows,
        other => vec![other],
    };
    Some(
        rows.into_iter()
            .filter_map(|row| {
                let name = row.get("Name")?.as_str()?.trim();
                if name.is_empty() {
                    return None;
                }
                let vendor = row
                    .get("AdapterCompatibility")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                Some(AiHardwareDevice {
                    name: name.into(),
                    vendor: vendor.to_string(),
                    cuda: String::new(),
                    directml: String::new(),
                    capability: String::new(),
                    vram: String::new(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_measured_hardware() {
        let actual = parse_nvidia_smi("NVIDIA GeForce RTX 4060, 8192, 8.9\n");
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].name, "NVIDIA GeForce RTX 4060");
        assert_eq!(actual[0].cuda, "cuda:0");
        assert_eq!(actual[0].capability, "SM_8.9");
        assert_eq!(actual[0].vram, "8.0 GB");
        assert!(parse_nvidia_smi("unavailable").is_empty());
    }
}

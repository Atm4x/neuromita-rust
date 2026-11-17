use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSettings {
    pub enabled: bool,
    pub endpoint: String,
    pub allow_write: bool,
    pub allow_content: bool,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://127.0.0.1:8766/mcp".into(),
            allow_write: false,
            allow_content: false,
        }
    }
}

impl McpSettings {
    pub fn validate(&self) -> Result<(), super::SettingsError> {
        self.address().map(|_| ())
    }

    pub fn address(&self) -> Result<(std::net::SocketAddr, String), super::SettingsError> {
        let invalid = || {
            super::SettingsError::Validation(
                "MCP requires an HTTP loopback IP, explicit port and path".into(),
            )
        };
        let url = reqwest::Url::parse(&self.endpoint).map_err(|_| invalid())?;
        let ip = url
            .host_str()
            .ok_or_else(invalid)?
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .map_err(|_| invalid())?;
        let port = url.port().filter(|port| *port != 0).ok_or_else(invalid)?;
        if url.scheme() != "http"
            || !ip.is_loopback()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() == "/"
        {
            return Err(invalid());
        }
        Ok((std::net::SocketAddr::new(ip, port), url.path().to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled_and_only_explicit_loopback_endpoints_are_valid() {
        let settings = McpSettings::default();
        assert!(!settings.enabled);
        assert_eq!(settings.address().unwrap().1, "/mcp");
        for endpoint in [
            "https://127.0.0.1:8766/mcp",
            "http://0.0.0.0:8766/mcp",
            "http://127.0.0.1/mcp",
            "http://127.0.0.1:8766/",
            "http://127.0.0.1:8766/mcp?token=x",
        ] {
            assert!(
                McpSettings {
                    endpoint: endpoint.into(),
                    ..settings.clone()
                }
                .validate()
                .is_err(),
                "{endpoint}"
            );
        }
        assert!(
            McpSettings {
                endpoint: "http://[::1]:8766/local-mcp".into(),
                ..settings
            }
            .validate()
            .is_ok()
        );
    }
}

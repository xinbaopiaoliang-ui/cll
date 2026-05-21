use crate::config::NodeConfig;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdentityState {
    pub node_id: Option<u64>,
    pub panel_url: Option<String>,
    pub node_secret_present: bool,
    pub bootstrap_response_file: Option<String>,
    pub created_by_installer: bool,
    #[serde(skip_serializing)]
    node_secret: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IdentityFile {
    node_id: Option<u64>,
    panel_url: Option<String>,
    node_secret: Option<String>,
    bootstrap_response_file: Option<String>,
    created_by_installer: Option<bool>,
}

impl IdentityState {
    pub fn from_config(config: &NodeConfig) -> anyhow::Result<Self> {
        let identity_file = Path::new(&config.identity.identity_file);

        if identity_file.exists() {
            let content = fs::read_to_string(identity_file)
                .with_context(|| format!("failed to read {}", identity_file.display()))?;
            let file: IdentityFile = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", identity_file.display()))?;
            let node_secret = file.node_secret.filter(|secret| !secret.trim().is_empty());

            return Ok(Self {
                node_id: file.node_id.or(config.identity.node_id),
                panel_url: file.panel_url.or_else(|| config.identity.panel_url.clone()),
                node_secret_present: node_secret.is_some(),
                node_secret,
                bootstrap_response_file: file.bootstrap_response_file,
                created_by_installer: file.created_by_installer.unwrap_or(false),
            });
        }

        Ok(Self {
            node_id: config.identity.node_id,
            panel_url: config.identity.panel_url.clone(),
            node_secret_present: false,
            bootstrap_response_file: config
                .bootstrap
                .as_ref()
                .map(|bootstrap| bootstrap.response_file.clone()),
            created_by_installer: false,
            node_secret: None,
        })
    }

    pub fn is_bootstrap_placeholder(&self) -> bool {
        self.node_id.is_none() || !self.node_secret_present
    }

    pub fn control_plane_credentials(&self) -> Option<(u64, &str, &str)> {
        Some((
            self.node_id?,
            self.panel_url.as_deref()?,
            self.node_secret.as_deref()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{IdentityConfig, NodeConfig, RuntimeConfig};
    use std::net::SocketAddr;

    fn config(identity_file: String) -> NodeConfig {
        NodeConfig {
            identity: IdentityConfig {
                node_id: Some(7),
                panel_url: Some("https://panel.example.net".to_string()),
                identity_file,
            },
            runtime: RuntimeConfig {
                data_dir: "/tmp/xaccel-node".to_string(),
                log_dir: "/tmp/xaccel-node/log".to_string(),
                health_addr: "127.0.0.1:9876".parse::<SocketAddr>().unwrap(),
                channel: "stable".to_string(),
            },
            bootstrap: None,
            control: None,
            network: None,
            report: None,
            limits: None,
        }
    }

    #[test]
    fn treats_empty_secret_as_missing() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let identity_file = temp_dir.path().join("identity.json");
        std::fs::write(
            &identity_file,
            r#"{"node_id":7,"panel_url":"https://panel.example.net","node_secret":""}"#,
        )
        .expect("write identity");

        let state = IdentityState::from_config(&config(identity_file.display().to_string()))
            .expect("identity loads");

        assert!(state.is_bootstrap_placeholder());
        assert!(state.control_plane_credentials().is_none());
    }

    #[test]
    fn returns_control_plane_credentials_when_secret_exists() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let identity_file = temp_dir.path().join("identity.json");
        std::fs::write(
            &identity_file,
            r#"{"node_id":7,"panel_url":"https://panel.example.net","node_secret":"secret"}"#,
        )
        .expect("write identity");

        let state = IdentityState::from_config(&config(identity_file.display().to_string()))
            .expect("identity loads");
        let credentials = state.control_plane_credentials().expect("credentials");

        assert_eq!(credentials.0, 7);
        assert_eq!(credentials.1, "https://panel.example.net");
        assert_eq!(credentials.2, "secret");
    }
}

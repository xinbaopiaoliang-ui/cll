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

            return Ok(Self {
                node_id: file.node_id.or(config.identity.node_id),
                panel_url: file.panel_url.or_else(|| config.identity.panel_url.clone()),
                node_secret_present: file.node_secret.is_some(),
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
        })
    }

    pub fn is_bootstrap_placeholder(&self) -> bool {
        self.node_id.is_none() || !self.node_secret_present
    }
}


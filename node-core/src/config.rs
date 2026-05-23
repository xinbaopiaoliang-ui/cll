use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::Path,
};
use toml::value::Table;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeConfig {
    pub identity: IdentityConfig,
    pub runtime: RuntimeConfig,
    pub bootstrap: Option<BootstrapConfig>,
    pub control: Option<ControlPlaneConfig>,
    pub network: Option<NetworkConfig>,
    pub report: Option<ReportConfig>,
    pub limits: Option<LimitConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdentityConfig {
    pub node_id: Option<u64>,
    pub panel_url: Option<String>,
    pub identity_file: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeConfig {
    pub data_dir: String,
    pub log_dir: String,
    pub health_addr: SocketAddr,
    pub channel: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BootstrapConfig {
    pub response_file: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ControlPlaneConfig {
    pub enabled: bool,
    pub config_revision: u64,
    pub request_timeout_sec: u64,
    pub config_poll_interval_sec: u64,
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            config_revision: 1,
            request_timeout_sec: 5,
            config_poll_interval_sec: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    pub server_ip: String,
    pub listen_ip: Option<String>,
    pub server_port: u16,
    pub relay_server_ip: Option<String>,
    pub relay_server_port: Option<u16>,
    pub is_support_ipv6: bool,
    pub disable_quic: bool,
    pub area: String,
    pub bandwidth_quality: BandwidthQuality,
    pub tag: Option<String>,
    pub operator_ips: Option<OperatorIps>,
}

impl NetworkConfig {
    pub fn listen_host(&self) -> &str {
        self.listen_ip
            .as_deref()
            .map(str::trim)
            .filter(|listen_ip| !listen_ip.is_empty())
            .unwrap_or_else(|| self.server_ip.trim())
    }

    pub fn listen_endpoint(&self) -> String {
        socket_endpoint(self.listen_host(), self.server_port)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BandwidthQuality {
    Fast,
    Normal,
    Slow,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OperatorIps {
    pub telecom_ip: Option<String>,
    pub mobile_ip: Option<String>,
    pub unicom_ip: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReportConfig {
    pub interval_sec: u64,
    pub traffic_batch_sec: u64,
    pub metrics_interval_sec: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LimitConfig {
    pub max_sessions: u64,
    pub max_sessions_per_user: u64,
    pub max_udp_mappings: u64,
    pub default_user_speed_mbps: u64,
}

impl NodeConfig {
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse TOML {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.identity.identity_file.trim().is_empty() {
            bail!("identity.identity_file is required");
        }

        if self.runtime.data_dir.trim().is_empty() {
            bail!("runtime.data_dir is required");
        }

        if self.runtime.log_dir.trim().is_empty() {
            bail!("runtime.log_dir is required");
        }

        if self.runtime.channel.trim().is_empty() {
            bail!("runtime.channel is required");
        }

        if let Some(network) = &self.network {
            if network.server_ip.trim().is_empty() {
                bail!("network.server_ip is required");
            }

            if let Some(listen_ip) = &network.listen_ip {
                let listen_ip = listen_ip.trim();
                if listen_ip.is_empty() {
                    bail!("network.listen_ip must not be empty when provided");
                }

                listen_ip.parse::<IpAddr>().with_context(|| {
                    format!("network.listen_ip must be an IP address: {listen_ip}")
                })?;
            }

            if network.server_port == 0 {
                bail!("network.server_port must be greater than 0");
            }
        }

        if let Some(control) = &self.control {
            if control.request_timeout_sec == 0 {
                bail!("control.request_timeout_sec must be greater than 0");
            }
            if control.config_poll_interval_sec == 0 {
                bail!("control.config_poll_interval_sec must be greater than 0");
            }
        }

        Ok(())
    }
}

pub fn persist_remote_network_config(
    path: impl AsRef<Path>,
    config_revision: u64,
    network: &NetworkConfig,
) -> anyhow::Result<()> {
    let path = path.as_ref();
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut value = content
        .parse::<toml::Value>()
        .with_context(|| format!("failed to parse TOML {}", path.display()))?;
    let root = value
        .as_table_mut()
        .context("node config TOML root must be a table")?;

    let control = table_entry(root, "control")?;
    control.insert(
        "config_revision".to_string(),
        toml::Value::Integer(config_revision.min(i64::MAX as u64) as i64),
    );

    let network_table = table_entry(root, "network")?;
    network_table.insert(
        "server_ip".to_string(),
        toml::Value::String(network.server_ip.clone()),
    );
    set_optional_string(network_table, "listen_ip", network.listen_ip.as_deref());
    network_table.insert(
        "server_port".to_string(),
        toml::Value::Integer(i64::from(network.server_port)),
    );
    set_optional_string(
        network_table,
        "relay_server_ip",
        network.relay_server_ip.as_deref(),
    );
    set_optional_integer(
        network_table,
        "relay_server_port",
        network.relay_server_port.map(i64::from),
    );
    network_table.insert(
        "is_support_ipv6".to_string(),
        toml::Value::Boolean(network.is_support_ipv6),
    );
    network_table.insert(
        "disable_quic".to_string(),
        toml::Value::Boolean(network.disable_quic),
    );
    network_table.insert(
        "area".to_string(),
        toml::Value::String(network.area.clone()),
    );
    network_table.insert(
        "bandwidth_quality".to_string(),
        toml::Value::String(
            match &network.bandwidth_quality {
                BandwidthQuality::Fast => "fast",
                BandwidthQuality::Normal => "normal",
                BandwidthQuality::Slow => "slow",
            }
            .to_string(),
        ),
    );
    set_optional_string(network_table, "tag", network.tag.as_deref());

    if let Some(operator_ips) = &network.operator_ips {
        let operator_table = table_entry(network_table, "operator_ips")?;
        set_optional_string(
            operator_table,
            "telecom_ip",
            operator_ips.telecom_ip.as_deref(),
        );
        set_optional_string(
            operator_table,
            "mobile_ip",
            operator_ips.mobile_ip.as_deref(),
        );
        set_optional_string(
            operator_table,
            "unicom_ip",
            operator_ips.unicom_ip.as_deref(),
        );
    } else {
        network_table.remove("operator_ips");
    }

    let content =
        toml::to_string_pretty(&value).context("failed to encode updated node config TOML")?;
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn table_entry<'a>(table: &'a mut Table, key: &str) -> anyhow::Result<&'a mut Table> {
    let value = table
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(Table::new()));
    value
        .as_table_mut()
        .with_context(|| format!("[{key}] must be a table"))
}

fn set_optional_string(table: &mut Table, key: &str, value: Option<&str>) {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            table.insert(key.to_string(), toml::Value::String(value.to_string()));
        }
        None => {
            table.remove(key);
        }
    }
}

fn set_optional_integer(table: &mut Table, key: &str, value: Option<i64>) {
    match value {
        Some(value) => {
            table.insert(key.to_string(), toml::Value::Integer(value));
        }
        None => {
            table.remove(key);
        }
    }
}

fn socket_endpoint(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_installer_config() {
        let config: NodeConfig = toml::from_str(
            r#"
            [identity]
            identity_file = "/var/lib/xaccel-node/identity.json"

            [runtime]
            data_dir = "/var/lib/xaccel-node"
            log_dir = "/var/log/xaccel-node"
            health_addr = "127.0.0.1:9876"
            channel = "stable"

            [bootstrap]
            response_file = "/var/lib/xaccel-node/bootstrap-response.json"

            [control]
            enabled = false
            "#,
        )
        .expect("config parses");

        config.validate().expect("config validates");
    }

    #[test]
    fn network_listen_endpoint_prefers_listen_ip() {
        let network = NetworkConfig {
            server_ip: "47.83.160.126".to_string(),
            listen_ip: Some("0.0.0.0".to_string()),
            server_port: 666,
            relay_server_ip: None,
            relay_server_port: None,
            is_support_ipv6: false,
            disable_quic: false,
            area: "UNKNOWN".to_string(),
            bandwidth_quality: BandwidthQuality::Normal,
            tag: None,
            operator_ips: None,
        };

        assert_eq!(network.listen_endpoint(), "0.0.0.0:666");
    }

    #[test]
    fn network_listen_endpoint_falls_back_to_server_ip() {
        let network = NetworkConfig {
            server_ip: "103.201.131.99".to_string(),
            listen_ip: None,
            server_port: 666,
            relay_server_ip: None,
            relay_server_port: None,
            is_support_ipv6: false,
            disable_quic: false,
            area: "UNKNOWN".to_string(),
            bandwidth_quality: BandwidthQuality::Normal,
            tag: None,
            operator_ips: None,
        };

        assert_eq!(network.listen_endpoint(), "103.201.131.99:666");
    }

    #[test]
    fn persists_remote_network_config_for_next_restart() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
            [identity]
            identity_file = "/tmp/xaccel-node/identity.json"

            [runtime]
            data_dir = "/tmp/xaccel-node"
            log_dir = "/tmp/xaccel-node/log"
            health_addr = "127.0.0.1:9876"
            channel = "stable"

            [control]
            enabled = true
            config_revision = 1
            request_timeout_sec = 5
            config_poll_interval_sec = 30

            [network]
            server_ip = "103.201.131.99"
            listen_ip = "0.0.0.0"
            server_port = 666
            is_support_ipv6 = false
            disable_quic = false
            area = "UNKNOWN"
            bandwidth_quality = "normal"
            tag = "standalone"
            "#,
        )
        .expect("write config");
        let network = NetworkConfig {
            server_ip: "47.83.160.126".to_string(),
            listen_ip: Some("0.0.0.0".to_string()),
            server_port: 667,
            relay_server_ip: None,
            relay_server_port: None,
            is_support_ipv6: true,
            disable_quic: true,
            area: "HK".to_string(),
            bandwidth_quality: BandwidthQuality::Fast,
            tag: Some("premium".to_string()),
            operator_ips: Some(OperatorIps {
                telecom_ip: Some("47.83.160.126".to_string()),
                mobile_ip: None,
                unicom_ip: None,
            }),
        };

        persist_remote_network_config(&config_path, 7, &network).expect("config persisted");
        let persisted = NodeConfig::from_file(&config_path).expect("persisted config parses");
        let control = persisted.control.expect("control");
        let network = persisted.network.expect("network");

        assert_eq!(control.config_revision, 7);
        assert_eq!(network.server_ip, "47.83.160.126");
        assert_eq!(network.server_port, 667);
        assert_eq!(network.area, "HK");
        assert!(network.is_support_ipv6);
        assert!(network.disable_quic);
        assert!(matches!(network.bandwidth_quality, BandwidthQuality::Fast));
        assert_eq!(network.tag.as_deref(), Some("premium"));
        assert_eq!(
            network
                .operator_ips
                .as_ref()
                .and_then(|ips| ips.telecom_ip.as_deref()),
            Some("47.83.160.126")
        );
    }
}

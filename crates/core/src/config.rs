use crate::error::AgentError;
use anyhow::Context;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub listen: ListenConfig,
    pub capture: CaptureConfig,
    pub upload: UploadConfig,
    pub storage: StorageConfig,
    pub log: LogConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListenConfig {
    pub socks5: Socks5ListenConfig,
    #[serde(default)]
    pub http: HttpListenConfig,
    pub tcp: Vec<TcpForwardConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Socks5ListenConfig {
    pub enabled: bool,
    pub bind: String,
    pub auth: Option<Socks5AuthConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Socks5AuthConfig {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpListenConfig {
    pub enabled: bool,
    pub bind: String,
    pub auth: Option<HttpProxyAuthConfig>,
}

impl Default for HttpListenConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "0.0.0.0:1081".to_string(),
            auth: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpProxyAuthConfig {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TcpForwardConfig {
    pub name: String,
    pub bind: String,
    pub remote: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaptureConfig {
    pub targets: Vec<CaptureTarget>,
    pub save_dir: String,
    pub save_uncaptured_sessions: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaptureTarget {
    pub ip: String,
    pub ports: Option<Vec<u16>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub basic_auth: BasicAuthConfig,
    pub auto_package_on_disconnect: bool,
    pub upload_interval_seconds: u64,
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    pub retry: RetryConfig,
}

fn default_request_timeout_seconds() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize)]
pub struct BasicAuthConfig {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub base_delay_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub flush_each_chunk: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogConfig {
    pub level: String,
}

impl AppConfig {
    pub fn load_from_path(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("failed to read config {}", path.as_ref().display()))?;
        Self::from_yaml_str(&text)
    }

    pub fn from_yaml_str(text: &str) -> anyhow::Result<Self> {
        let config = serde_yaml::from_str::<Self>(text)
            .map_err(|error| AgentError::InvalidConfig(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        self.upload.validate()
    }
}

impl UploadConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.upload_interval_seconds < 60 {
            return Err(AgentError::UploadIntervalTooSmall.into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_YAML_WITHOUT_REMOVED_FIELDS: &str = r#"listen:
  socks5:
    enabled: true
    bind: "0.0.0.0:1080"
  tcp:
    - name: "game-server-9000"
      bind: "0.0.0.0:9000"
      remote: "1.2.3.4:9000"
capture:
  targets:
    - ip: "1.2.3.4"
      ports: [9000, 9001]
  save_dir: "./captures"
  save_uncaptured_sessions: false
upload:
  enabled: true
  endpoint: "http://127.0.0.1:8080/api/client/capture/upload"
  basic_auth:
    username: "agent"
    password: "change-me"
  auto_package_on_disconnect: true
  upload_interval_seconds: 60
  retry:
    max_attempts: 5
    base_delay_seconds: 3
storage:
  flush_each_chunk: true
log:
  level: "info"
"#;

    #[test]
    fn parses_valid_config_without_package_format_or_upload_dirs() {
        let config = AppConfig::from_yaml_str(VALID_YAML_WITHOUT_REMOVED_FIELDS).unwrap();

        assert!(config.storage.flush_each_chunk);
    }

    #[test]
    fn rejects_upload_interval_below_sixty() {
        let yaml = VALID_YAML_WITHOUT_REMOVED_FIELDS
            .replace("upload_interval_seconds: 60", "upload_interval_seconds: 59");

        let error = AppConfig::from_yaml_str(&yaml).expect_err("interval below 60 should fail");

        assert!(
            error
                .to_string()
                .contains("upload interval must be at least 60 seconds"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn accepts_upload_interval_equal_to_sixty() {
        let config = AppConfig::from_yaml_str(VALID_YAML_WITHOUT_REMOVED_FIELDS)
            .expect("60 second interval should pass");

        assert_eq!(config.upload.upload_interval_seconds, 60);
    }

    #[test]
    fn rejects_removed_upload_directory_keys() {
        let yaml = VALID_YAML_WITHOUT_REMOVED_FIELDS.replace(
            "  retry:\n    max_attempts: 5\n    base_delay_seconds: 3",
            "  queue_dir: \"./upload_queue\"\n  uploaded_dir: \"./uploaded\"\n  failed_dir: \"./failed\"\n  retry:\n    max_attempts: 5\n    base_delay_seconds: 3",
        );

        let error = AppConfig::from_yaml_str(&yaml).expect_err("removed upload keys should fail");

        assert!(
            error.to_string().contains("unknown field `queue_dir`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_removed_storage_package_format_key() {
        let yaml = VALID_YAML_WITHOUT_REMOVED_FIELDS.replace(
            "storage:\n  flush_each_chunk: true",
            "storage:\n  flush_each_chunk: true\n  package_format: \"zip\"",
        );

        let error = AppConfig::from_yaml_str(&yaml).expect_err("removed storage key should fail");

        assert!(
            error.to_string().contains("unknown field `package_format`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_removed_agent_section() {
        let yaml = format!(
            "agent:\n  device_id: \"local-dev-device\"\n  user_id: \"\"\n{VALID_YAML_WITHOUT_REMOVED_FIELDS}"
        );

        let error = AppConfig::from_yaml_str(&yaml).expect_err("removed agent section should fail");

        assert!(
            error.to_string().contains("unknown field `agent`"),
            "unexpected error: {error}"
        );
    }
}

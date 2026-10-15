use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AppConfig {
    pub secret_key: String,
    /// Devices whose backups run automatically (Whitelist).
    #[serde(default)]
    pub approved_uuids: Vec<String>,
    /// Devices to never prompt about again (IgnoreForever).
    #[serde(default)]
    pub ignored_uuids: Vec<String>,
    /// Devices that prompt for confirmation on every plug-in (AskEachTime).
    #[serde(default)]
    pub ask_each_time_uuids: Vec<String>,
}

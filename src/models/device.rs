use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum DeviceAction {
    Whitelist,
    IgnoreForever,
    AskEachTime,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BackupRule {
    pub source_path: String,
    pub destination_path: String,
    pub exclude: Vec<String>,
    #[serde(default = "default_delete")]
    pub delete_missing: bool,
}

fn default_delete() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeviceConfig {
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub action: DeviceAction,
    pub backup_rules: Vec<BackupRule>,
}

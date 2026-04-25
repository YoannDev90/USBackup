use crate::models::config::AppConfig;
use crate::models::device::DeviceConfig;
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_PATH: &str = "backup_config.toml";

pub fn load_config() -> AppConfig {
    if Path::new(CONFIG_PATH).exists() {
        match fs::read_to_string(CONFIG_PATH) {
            Ok(content) => toml::from_str(&content).unwrap_or_else(|_| AppConfig::default()),
            Err(_) => AppConfig::default(),
        }
    } else {
        AppConfig::default()
    }
}

pub fn save_config(config: &AppConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let content = toml::to_string_pretty(config)?;
    fs::write(CONFIG_PATH, content)?;
    Ok(())
}

pub fn load_device_config(mount_point: &Path) -> Option<DeviceConfig> {
    let config_path = mount_point.join(".usbackup.toml");
    if config_path.exists() {
        match fs::read_to_string(config_path) {
            Ok(content) => toml::from_str(&content).ok(),
            Err(_) => None,
        }
    } else {
        None
    }
}

pub fn save_device_config(
    mount_point: &Path,
    config: &DeviceConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config_path = mount_point.join(".usbackup.toml");
    let content = toml::to_string_pretty(config)?;
    fs::write(config_path, content)?;
    Ok(())
}

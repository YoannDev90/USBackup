use crate::models::config::AppConfig;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub const CONFIG_PATH: &str = "backup_config.json";

pub fn load_config() -> AppConfig {
    if Path::new(CONFIG_PATH).exists() {
        match fs::read_to_string(CONFIG_PATH) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|_| AppConfig {
                devices: HashMap::new(),
            }),
            Err(_) => AppConfig {
                devices: HashMap::new(),
            },
        }
    } else {
        AppConfig {
            devices: HashMap::new(),
        }
    }
}

pub fn save_config(config: &AppConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let content = serde_json::to_string_pretty(config)?;
    fs::write(CONFIG_PATH, content)?;
    Ok(())
}

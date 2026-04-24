use std::fs;
use std::path::Path;
use std::collections::HashMap;
use crate::models::config::AppConfig;

pub const CONFIG_PATH: &str = "backup_config.json";

pub fn load_config() -> AppConfig {
    if Path::new(CONFIG_PATH).exists() {
        let content = fs::read_to_string(CONFIG_PATH).expect("Erreur de lecture config");
        serde_json::from_str(&content).unwrap_or(AppConfig {
            devices: HashMap::new(),
        })
    } else {
        AppConfig {
            devices: HashMap::new(),
        }
    }
}

pub fn save_config(config: &AppConfig) {
    let content = serde_json::to_string_pretty(config).expect("Erreur de sérialisation");
    fs::write(CONFIG_PATH, content).expect("Erreur d'écriture config");
}

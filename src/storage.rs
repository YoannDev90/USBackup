use crate::models::config::AppConfig;
use crate::models::device::DeviceConfig;
use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use sha2::Sha256;
use std::fs;
use std::path::Path;

pub const CONFIG_PATH: &str = "backup_config.toml";

pub fn load_config() -> AppConfig {
    if Path::new(CONFIG_PATH).exists() {
        match fs::read_to_string(CONFIG_PATH) {
            Ok(content) => {
                match toml::from_str(&content) {
                    Ok(mut config) => {
                        // Générer une clé secrète si elle n'existe pas
                        if let AppConfig { secret_key, .. } = &config {
                            if secret_key.is_empty() {
                                let mut key = [0u8; 32];
                                rand::rng().fill(&mut key);
                                config.secret_key = hex::encode(key);
                                let _ = save_config(&config);
                            }
                        }
                        config
                    }
                    Err(e) => {
                        eprintln!("Erreur lors du parsing de {} : {}. Utilisation de la config par défaut.", CONFIG_PATH, e);
                        generate_default_config()
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "Erreur lors de la lecture de {} : {}. Utilisation de la config par défaut.",
                    CONFIG_PATH, e
                );
                generate_default_config()
            }
        }
    } else {
        generate_default_config()
    }
}

fn generate_default_config() -> AppConfig {
    let mut config = AppConfig::default();
    let mut key = [0u8; 32];
    rand::rng().fill(&mut key);
    config.secret_key = hex::encode(key);
    let _ = save_config(&config);
    config
}

pub fn save_config(config: &AppConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let content = toml::to_string_pretty(config)?;
    fs::write(CONFIG_PATH, content)?;
    Ok(())
}

pub fn sign_config(config: &mut DeviceConfig, secret_key: &str) {
    let mut config_for_signing = config.clone();
    config_for_signing.signature = None; // Ne pas signer la signature elle-même
    let serialized = toml::to_string(&config_for_signing).unwrap_or_default();

    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret_key.as_bytes()).expect("HMAC can take key of any size");
    mac.update(serialized.as_bytes());
    let result = mac.finalize();
    config.signature = Some(hex::encode(result.into_bytes()));
}

pub fn verify_signature(config: &DeviceConfig, secret_key: &str) -> bool {
    let signature_to_check = match &config.signature {
        Some(s) => s,
        None => return false,
    };

    let mut config_for_signing = config.clone();
    config_for_signing.signature = None;
    let serialized = toml::to_string(&config_for_signing).unwrap_or_default();

    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret_key.as_bytes()).expect("HMAC can take key of any size");
    mac.update(serialized.as_bytes());

    let expected_signature = hex::encode(mac.finalize().into_bytes());
    expected_signature == *signature_to_check
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

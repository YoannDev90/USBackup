use crate::models::config::AppConfig;
use crate::models::device::DeviceConfig;
use directories::ProjectDirs;
use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fs;
use std::path::{Path, PathBuf};

/// A device seen by the daemon without an interactive TTY. Stored so the
/// user can later run `usbackup approve <uuid>` from a terminal.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PendingDevice {
    pub uuid: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub product: String,
}

pub fn get_config_dir() -> PathBuf {
    if let Some(proj_dirs) = ProjectDirs::from("com", "usbackup", "USBackup") {
        let config_dir = proj_dirs.config_dir();
        if !config_dir.exists() {
            let _ = fs::create_dir_all(config_dir);
        }
        return config_dir.to_path_buf();
    }
    PathBuf::from(".")
}

pub fn get_config_path() -> PathBuf {
    get_config_dir().join("backup_config.toml")
}

fn pending_path() -> PathBuf {
    get_config_dir().join("pending_devices.toml")
}

pub fn load_pending() -> Vec<PendingDevice> {
    let path = pending_path();
    if !path.exists() {
        return Vec::new();
    }
    fs::read_to_string(&path)
        .ok()
        .and_then(|c| toml::from_str(&c).ok())
        .unwrap_or_default()
}

pub fn save_pending(list: &[PendingDevice]) {
    let path = pending_path();
    if let Ok(content) = toml::to_string_pretty(list) {
        let _ = fs::write(path, content);
    }
}

/// Add a device to the pending list (deduplicated by uuid).
pub fn add_pending(device: PendingDevice) {
    let mut list = load_pending();
    if !list.iter().any(|p| p.uuid == device.uuid) {
        list.push(device);
        save_pending(&list);
    }
}

/// Remove a uuid from the pending list. Returns true if it was present.
pub fn remove_pending(uuid: &str) -> bool {
    let mut list = load_pending();
    let before = list.len();
    list.retain(|p| p.uuid != uuid);
    if list.len() != before {
        save_pending(&list);
        true
    } else {
        false
    }
}

pub fn load_config() -> AppConfig {
    let config_path = get_config_path();
    if !config_path.exists() {
        return generate_default_config();
    }
    match fs::read_to_string(&config_path) {
        Ok(content) => match toml::from_str::<AppConfig>(&content) {
            Ok(mut config) => {
                // Générer une clé secrète si elle n'existe pas
                if config.secret_key.is_empty() {
                    let mut key = [0u8; 32];
                    rand::rng().fill(&mut key);
                    config.secret_key = hex::encode(key);
                    let _ = save_config(&config);
                }
                config
            }
            Err(e) => {
                // Ne JAMAIS écraser une config corrompue : perte de la clé secrète
                // et de tous les UUIDs approuvés, et invalidation de toutes les
                // signatures des clés USB. On repart d'une config en mémoire.
                eprintln!(
                    "ERROR: failed to parse {:?}: {}. Keeping the file untouched; using an in-memory default (approved devices and secret key are NOT loaded). Fix or delete the file manually.",
                    config_path, e
                );
                AppConfig::default()
            }
        },
        Err(e) => {
            eprintln!(
                "ERROR: failed to read {:?}: {}. Keeping the file untouched; using an in-memory default.",
                config_path, e
            );
            AppConfig::default()
        }
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
    let path = get_config_path();
    let content = toml::to_string_pretty(config)?;
    fs::write(&path, content)?;
    // The file contains the HMAC secret key: owner-read/write only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
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

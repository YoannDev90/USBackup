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

/// TOML has no top-level array value; wrap the list so `[[pending]]`
/// table-array syntax is used on disk.
#[derive(Serialize, Deserialize)]
struct PendingFile {
    #[serde(default)]
    pending: Vec<PendingDevice>,
}

pub fn load_pending() -> Vec<PendingDevice> {
    let path = pending_path();
    if !path.exists() {
        return Vec::new();
    }
    fs::read_to_string(&path)
        .ok()
        .and_then(|c| toml::from_str::<PendingFile>(&c).ok())
        .map(|f| f.pending)
        .unwrap_or_default()
}

pub fn save_pending(list: &[PendingDevice]) {
    let path = pending_path();
    let file = PendingFile {
        pending: list.to_vec(),
    };
    if let Ok(content) = toml::to_string_pretty(&file) {
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
                // Generate a secret key if one does not exist yet
                if config.secret_key.is_empty() {
                    let mut key = [0u8; 32];
                    rand::rng().fill(&mut key);
                    config.secret_key = hex::encode(key);
                    let _ = save_config(&config);
                }
                config
            }
            Err(e) => {
                // NEVER overwrite a corrupt config: that would lose the secret key
                // and every approved UUID, and invalidate every signature on
                // USB keys. Fall back to an in-memory default instead.
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

/// Deterministic byte representation of a DeviceConfig used as HMAC input.
///
/// Deliberately NOT `toml::to_string`: the toml crate's output (key order,
/// spacing, number formatting) is not a stability guarantee across versions.
/// A toml upgrade could therefore invalidate every signature on every USB
/// key. This function owns the format: fixed field order, one `key=value`
/// per line, rules separated by a record separator.
pub fn canonical_form(config: &DeviceConfig) -> String {
    let mut out = String::new();
    out.push_str(&format!("name={}\n", config.name));
    out.push_str(&format!("vendor_id={}\n", config.vendor_id));
    out.push_str(&format!("product_id={}\n", config.product_id));
    out.push_str(&format!(
        "uuid={}\n",
        config.uuid.as_deref().unwrap_or("")
    ));
    out.push_str(&format!("action={:?}\n", config.action));
    for rule in &config.backup_rules {
        out.push_str("rule\n");
        out.push_str(&format!("source_path={}\n", rule.source_path));
        out.push_str(&format!("destination_path={}\n", rule.destination_path));
        // U+001F unit separator: cannot appear in gitignore patterns from
        // the bundled list in any meaningful way, keeps joins unambiguous.
        out.push_str(&format!("exclude={}\n", rule.exclude.join("\u{1f}")));
        out.push_str(&format!("delete_missing={}\n", rule.delete_missing));
        out.push_str(&format!("incremental={}\n", rule.incremental));
        out.push_str(&format!("unmount_after={}\n", rule.unmount_after));
        out.push_str(&format!(
            "post_backup_script={}\n",
            rule.post_backup_script.as_deref().unwrap_or("")
        ));
        out.push_str("endrule\n");
    }
    out
}

pub fn sign_config(config: &mut DeviceConfig, secret_key: &str) {
    let mut config_for_signing = config.clone();
    config_for_signing.signature = None; // Never sign the signature itself
    let serialized = canonical_form(&config_for_signing);

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
    let serialized = canonical_form(&config_for_signing);

    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret_key.as_bytes()).expect("HMAC can take key of any size");
    mac.update(serialized.as_bytes());

    // Constant-time comparison via the hmac crate (rejects on any mismatch).
    let sig_bytes = match hex::decode(signature_to_check) {
        Ok(b) => b,
        Err(_) => return false,
    };
    mac.verify_slice(&sig_bytes).is_ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::device::{BackupRule, DeviceAction, DeviceConfig};

    fn sample_config() -> DeviceConfig {
        DeviceConfig {
            name: "Test Key".into(),
            vendor_id: 0x0951,
            product_id: 0x1666,
            uuid: Some("ABCD-1234".into()),
            signature: None,
            action: DeviceAction::Whitelist,
            backup_rules: vec![BackupRule {
                source_path: "/home/user/Documents".into(),
                destination_path: "backups/docs".into(),
                exclude: vec!["*.tmp".into(), "cache/".into()],
                delete_missing: true,
                incremental: true,
                post_backup_script: Some("notify-send done".into()),
                unmount_after: true,
            }],
        }
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let mut config = sample_config();
        sign_config(&mut config, "test-secret");
        assert!(config.signature.is_some());
        assert!(verify_signature(&config, "test-secret"));
    }

    #[test]
    fn verify_fails_with_wrong_key() {
        let mut config = sample_config();
        sign_config(&mut config, "test-secret");
        assert!(!verify_signature(&config, "other-secret"));
    }

    #[test]
    fn verify_fails_when_payload_tampered() {
        let mut config = sample_config();
        sign_config(&mut config, "test-secret");
        config.name = "Evil Key".into();
        assert!(!verify_signature(&config, "test-secret"));
    }

    #[test]
    fn verify_fails_when_rule_tampered() {
        let mut config = sample_config();
        sign_config(&mut config, "test-secret");
        config.backup_rules[0].source_path = "/etc".into();
        assert!(!verify_signature(&config, "test-secret"));
    }

    #[test]
    fn verify_fails_on_missing_or_invalid_signature() {
        let config = sample_config();
        assert!(!verify_signature(&config, "test-secret"));

        let mut bad = sample_config();
        bad.signature = Some("not-hex-zz".into());
        assert!(!verify_signature(&bad, "test-secret"));
    }

    #[test]
    fn verify_fails_on_wrong_length_signature() {
        let mut config = sample_config();
        sign_config(&mut config, "test-secret");
        // Truncate the signature
        let sig = config.signature.take().unwrap();
        config.signature = Some(sig[..10].to_string());
        assert!(!verify_signature(&config, "test-secret"));
    }

    #[test]
    fn canonical_form_is_stable_and_field_ordered() {
        let a = canonical_form(&sample_config());
        let b = canonical_form(&sample_config());
        assert_eq!(a, b);
        // Fixed order: name first, then vendor_id
        let name_pos = a.find("name=").unwrap();
        let vendor_pos = a.find("vendor_id=").unwrap();
        assert!(name_pos < vendor_pos);
        assert!(a.contains("rule\n"));
        assert!(a.contains("endrule\n"));
    }

    #[test]
    fn canonical_form_reflects_signature_exclusion() {
        // Signing input must ignore the signature field itself
        let mut signed = sample_config();
        sign_config(&mut signed, "k");
        let with_sig = canonical_form(&signed);
        let mut unsigned = signed.clone();
        unsigned.signature = None;
        assert_eq!(with_sig, canonical_form(&unsigned));
    }

    #[test]
    fn pending_roundtrip_helpers() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Wrapper {
            pending: Vec<PendingDevice>,
        }
        let p = PendingDevice {
            uuid: "X".into(),
            vendor_id: 1,
            product_id: 2,
            product: "p".into(),
        };
        let s = toml::to_string(&Wrapper {
            pending: vec![p],
        })
        .unwrap();
        let back: Wrapper = toml::from_str(&s).unwrap();
        assert_eq!(back.pending[0].uuid, "X");
        assert_eq!(back.pending[0].vendor_id, 1);
    }
}

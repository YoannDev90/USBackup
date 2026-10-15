use nusb::hotplug::HotplugEvent;

mod handler;
mod models;
mod notifications;
mod storage;

use crate::storage::load_config;
use log::{debug, error, info, warn};
use std::process::Command;

fn check_dependencies() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let deps = ["rsync", "udisksctl"];
    for dep in deps {
        if !Command::new("which").arg(dep).output()?.status.success() {
            error!("Dépendance manquante : {}", dep);
            return Err(format!("L'outil '{}' est requis mais n'est pas installé.", dep).into());
        }
    }
    Ok(())
}

fn install_systemd_service() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let home = std::env::var("HOME")?;
    let exe_path = std::env::current_exe()?;
    let service_dir = format!("{}/.config/systemd/user", home);
    let service_path = format!("{}/usbackup.service", service_dir);

    if std::path::Path::new(&service_path).exists() {
        return Ok(());
    }

    info!("Installation du service systemd utilisateur...");
    std::fs::create_dir_all(&service_dir)?;

    let template = include_str!("../usbackup.service");
    let service_content = template.replace("%EXE_PATH%", &exe_path.display().to_string());

    std::fs::write(&service_path, service_content)?;

    // Activer et démarrer le service
    Command::new("systemctl")
        .arg("--user")
        .arg("daemon-reload")
        .output()?;
    Command::new("systemctl")
        .arg("--user")
        .arg("enable")
        .arg("usbackup.service")
        .output()?;

    info!("Service systemd installé et activé avec succès.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialise le logger
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    // Vérifier les dépendances système
    check_dependencies()?;

    // Installer le service si nécessaire
    if let Err(e) = install_systemd_service() {
        warn!("Impossible d'installer le service systemd : {}", e);
    }

    info!("Service USBackup démarré. En attente de périphériques...");

    let watch = match nusb::watch_devices() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Erreur Initialisation : {}", e);
            return Ok(());
        }
    };

    use futures_lite::StreamExt;
    let mut stream = watch;

    while let Some(event) = stream.next().await {
        match event {
            HotplugEvent::Connected(device) => {
                let vid = device.vendor_id();
                let pid = device.product_id();
                let product = device.product_string().unwrap_or("Inconnu").to_string();
                let device_key = format!("{:04x}:{:04x}", vid, pid);

                info!("[+] Appareil détecté : {} ({})", product, device_key);

                // Recharger la config des UUID approuvés
                let config = load_config();

                // Find this device's UUID, correlating by VID/PID so we never
                // pick up a different plugged-in key.
                let mut device_uuid = None;
                for i in 0..5 {
                    if i > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                    }

                    let parts = crate::handler::udev_utils::find_usb_partitions();
                    debug!("Checking USB partitions: {:?}", parts);

                    for part in parts {
                        let ids_match = part.vendor_id == Some(vid) && part.product_id == Some(pid);
                        if ids_match {
                            if let Some(u) = part.uuid {
                                device_uuid = Some(u);
                                break;
                            }
                        }
                    }
                    if device_uuid.is_some() {
                        break;
                    }
                }

                if let Some(uuid) = device_uuid {
                    if config.ignored_uuids.contains(&uuid) {
                        debug!("Device {} is set to IgnoreForever; skipping.", uuid);
                        continue;
                    }
                    if config.approved_uuids.contains(&uuid) {
                        // Whitelist: run the backup automatically
                        tokio::spawn(async move {
                            // Wait for the key to mount so its local config can be read
                            if let Some(dev_conf) =
                                crate::handler::trigger_backup_by_uuid(vid, pid, &uuid).await
                            {
                                info!("Backup finished for {}", dev_conf.name);
                            }
                        });
                    } else if config.ask_each_time_uuids.contains(&uuid) {
                        // AskEachTime: prompt on every plug-in
                        let product_clone = product.clone();
                        let uuid_clone = uuid.clone();
                        tokio::task::spawn_blocking(move || {
                            let run_now = dialoguer::Confirm::new()
                                .with_prompt(format!(
                                    "Device '{}' ({}) is set to AskEachTime. Run a backup now?",
                                    product_clone, uuid
                                ))
                                .default(true)
                                .interact()
                                .unwrap_or(false);
                            if run_now {
                                tokio::spawn(async move {
                                    if let Some(dev_conf) =
                                        crate::handler::trigger_backup_by_uuid(
                                            vid, pid, &uuid_clone,
                                        )
                                        .await
                                    {
                                        info!("Backup finished for {}", dev_conf.name);
                                    }
                                });
                            }
                        });
                    } else {
                        // New (unapproved) device: pick a DeviceAction
                        let product_clone = product.clone();
                        tokio::task::spawn_blocking(move || {
                            let options = [
                                "Approve and configure backups (Whitelist)",
                                "Approve, but ask every time (AskEachTime)",
                                "Ignore this device forever (IgnoreForever)",
                                "Cancel",
                            ];
                            let selection = dialoguer::Select::new()
                                .with_prompt(format!(
                                    "New device '{}' detected ({}). Choose an action:",
                                    product_clone, uuid
                                ))
                                .items(&options)
                                .default(0)
                                .interact()
                                .unwrap_or(3);

                            let mut config = load_config();
                            match selection {
                                0 => {
                                    if !config.approved_uuids.contains(&uuid) {
                                        config.approved_uuids.push(uuid.clone());
                                        if let Err(e) = crate::storage::save_config(&config) {
                                            error!("Failed to save config: {}", e);
                                        }
                                    }

                                    let rt = tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                        .unwrap();
                                    rt.block_on(async {
                                        if let Err(e) = crate::handler::run_wizard(
                                            vid, pid, &product_clone, &uuid,
                                        )
                                        .await
                                        {
                                            error!("Wizard error: {}", e);
                                        }
                                    });
                                }
                                1 => {
                                    if !config.ask_each_time_uuids.contains(&uuid) {
                                        config.ask_each_time_uuids.push(uuid.clone());
                                        if let Err(e) = crate::storage::save_config(&config) {
                                            error!("Failed to save config: {}", e);
                                        }
                                    }
                                    info!("Device {} set to AskEachTime.", uuid);
                                }
                                2 => {
                                    if !config.ignored_uuids.contains(&uuid) {
                                        config.ignored_uuids.push(uuid.clone());
                                        if let Err(e) = crate::storage::save_config(&config) {
                                            error!("Failed to save config: {}", e);
                                        }
                                    }
                                    info!("Device {} set to IgnoreForever.", uuid);
                                }
                                _ => {
                                    debug!("Device {} approval cancelled.", uuid);
                                }
                            }
                        });
                    }
                } else {
                    warn!("Unable to extract UUID for device {}.", product);
                }
            }
            HotplugEvent::Disconnected(_) => {
                info!("[-] Appareil déconnecté");
            }
        }
    }

    Ok(())
}

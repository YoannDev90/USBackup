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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialise le logger
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    // Vérifier les dépendances système
    check_dependencies()?;

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

                // Chercher l'UUID de cet appareil avec plusieurs tentatives
                let mut device_uuid = None;
                for i in 0..5 {
                    if i > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    let parts = crate::handler::udev_utils::find_usb_partitions();
                    debug!("Parts found: {:?}", parts);
                    for part in parts {
                        if let Some(u) = crate::handler::udev_utils::get_partition_uuid(&part) {
                            device_uuid = Some(u);
                            break;
                        }
                    }
                    if device_uuid.is_some() {
                        break;
                    }
                }

                if let Some(uuid) = device_uuid {
                    if config.approved_uuids.contains(&uuid) {
                        // L'appareil est approuvé, on cherche sa config locale sur la clé
                        tokio::spawn(async move {
                            // On attend que la clé soit montée pour lire sa config
                            if let Some(dev_conf) =
                                crate::handler::trigger_backup_by_uuid(vid, pid, &uuid).await
                            {
                                info!("Backup terminée pour {}", dev_conf.name);
                            }
                        });
                    } else {
                        // Nouveau périphérique ou non approuvé
                        let product_clone = product.clone();
                        tokio::task::spawn_blocking(move || {
                            if dialoguer::Confirm::new()
                                .with_prompt(format!(
                                    "Nouveau périphérique ({}) détecté. Voulez-vous approuver cet UUID ({}) ?",
                                    product_clone, uuid
                                ))
                                .default(false)
                                .interact()
                                .unwrap_or(false)
                            {
                                let mut config = load_config();
                                if !config.approved_uuids.contains(&uuid) {
                                    config.approved_uuids.push(uuid.clone());
                                    if let Err(e) = crate::storage::save_config(&config) {
                                        error!("Erreur sauvegarde config : {}", e);
                                    }
                                }

                                let rt = tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                    .unwrap();
                                rt.block_on(async {
                                    if let Err(e) =
                                        crate::handler::run_wizard(vid, pid, &product_clone, &uuid)
                                            .await
                                    {
                                        error!("Erreur Wizard : {}", e);
                                    }
                                });
                            }
                        });
                    }
                } else {
                    warn!("Impossible d'extraire l'UUID pour l'appareil {}.", product);
                }
            }
            HotplugEvent::Disconnected(_) => {
                info!("[-] Appareil déconnecté");
            }
        }
    }

    Ok(())
}

use crate::notifications;
use log::{debug, error, info, warn};
use std::path::Path;
use sysinfo::Disks;
use tokio::process::Command as TokioCommand;

fn find_usb_partitions() -> Vec<String> {
    let mut partitions = Vec::new();
    let mut enumerator = match udev::Enumerator::new() {
        Ok(e) => e,
        Err(_) => return partitions,
    };

    let _ = enumerator.match_subsystem("block");
    let _ = enumerator.match_property("DEVTYPE", "partition");

    if let Ok(devices) = enumerator.scan_devices() {
        for device in devices {
            if let Some(devnode) = device.devnode() {
                debug!("udev examine : {:?}", devnode);
            }
            let mut current = Some(device.clone());
            let mut is_usb = false;
            while let Some(parent) = current {
                if let Some(bus) = parent.property_value("ID_BUS") {
                    if bus == "usb" {
                        is_usb = true;
                        break;
                    }
                }
                current = parent.parent();
            }

            if is_usb {
                if let Some(devnode) = device.devnode() {
                    debug!("Partition USB trouvée via udev : {:?}", devnode);
                    partitions.push(devnode.to_string_lossy().to_string());
                }
            }
        }
    }
    partitions
}

pub async fn trigger_backup(device_config: &crate::models::device::DeviceConfig) {
    info!("Préparation du backup pour {}", device_config.name);

    // Détection des disques via sysinfo (Cross-platform) avec retries
    let mut found_mount_point = None;
    let mut attempts = 0;

    debug!("Surveillance du montage (30 tentatives)...");

    while attempts < 30 && found_mount_point.is_none() {
        if attempts > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }

        // TENTER LE MONTAGE À CHAQUE ESSAI si non monté
        let usb_parts = find_usb_partitions();
        for part in usb_parts {
            // On tente de monter toutes les partitions USB trouvées via udisksctl (Linux)
            let status = TokioCommand::new("udisksctl")
                .arg("mount")
                .arg("-b")
                .arg(&part)
                .output()
                .await;

            if let Ok(out) = status {
                if out.status.success() {
                    let msg = String::from_utf8_lossy(&out.stdout);
                    info!("Montage réussi : {}", msg.trim());
                }
            }
        }

        let disks = Disks::new_with_refreshed_list();

        // Debug additionnel : affiche la sortie de lsblk pendant l'essai
        if attempts % 5 == 0 {
            let lsblk_out = TokioCommand::new("lsblk")
                .arg("-o")
                .arg("NAME,TRAN,MOUNTPOINT")
                .output()
                .await;
            if let Ok(out) = lsblk_out {
                debug!("LSBLK Snapshot:\n{}", String::from_utf8_lossy(&out.stdout));
            }
        }

        for disk in &disks {
            let name = disk.name().to_string_lossy();
            let mount_point = disk.mount_point().to_string_lossy();
            let file_system = disk.file_system().to_string_lossy();

            debug!(
                "[Essai {}] Disque trouvé: {} | Mount: {} | FS: {}",
                attempts + 1,
                name,
                mount_point,
                file_system
            );

            // Stratégie de détection améliorée
            let is_removable =
                mount_point.contains("/media/") || mount_point.contains("/run/media/");

            if mount_point
                .to_lowercase()
                .contains(&device_config.name.to_lowercase())
                || name
                    .to_lowercase()
                    .contains(&device_config.name.to_lowercase())
                || is_removable
            {
                found_mount_point = Some(disk.mount_point().to_path_buf());
                break;
            }
        }
        attempts += 1;
    }

    if let Some(path) = found_mount_point {
        info!("Disque détecté sur : {:?}", path);
    } else {
        warn!("Impossible de localiser le point de montage.");
        return;
    }

    notifications::notify_backup_start(&device_config.name);

    for rule in &device_config.backup_rules {
        info!(
            "Synchronisation {} vers {}",
            rule.source_path, rule.destination_path
        );

        // Vérifier si la source existe
        if !Path::new(&rule.source_path).exists() {
            error!("Source inexistante : {}", rule.source_path);
            continue;
        }

        // Créer le répertoire de destination s'il n'existe pas
        if let Err(e) = std::fs::create_dir_all(&rule.destination_path) {
            error!(
                "Impossible de créer la destination {} : {}",
                rule.destination_path, e
            );
            continue;
        }

        // Utilisation de rsync pour une synchronisation efficace
        let status = TokioCommand::new("rsync")
            .arg("-avz")
            .arg("--delete")
            .arg(&rule.source_path)
            .arg(&rule.destination_path)
            .status()
            .await;

        match status {
            Ok(s) if s.success() => info!("Succès pour {}", rule.source_path),
            Ok(s) => error!(
                "Rsync a échoué avec le code {} pour {}",
                s, rule.source_path
            ),
            Err(e) => error!("Erreur lors de l'exécution de rsync : {}", e),
        }
    }

    notifications::notify_backup_success(&device_config.name);
}

// Code pour ask_user_action supprimé car incompatible avec le mode TUI actuel.

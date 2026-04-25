use crate::models::device::DeviceAction;
use crate::notifications;
use colored::Colorize;
use log::{debug, info, warn};
use std::io::{self, Write};
use std::process::Command;
use sysinfo::Disks;

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

pub fn trigger_backup(device_config: &crate::models::device::DeviceConfig) {
    info!("Préparation du backup pour {}", device_config.name);

    // Détection des disques via sysinfo (Cross-platform) avec retries
    let mut found_mount_point = None;
    let mut attempts = 0;

    debug!("Surveillance du montage (8 tentatives)...");

    while attempts < 8 && found_mount_point.is_none() {
        if attempts > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }

        // TENTER LE MONTAGE À CHAQUE ESSAI si non monté
        let usb_parts = find_usb_partitions();
        for part in usb_parts {
            // On tente de monter toutes les partitions USB trouvées
            let status = Command::new("udisksctl")
                .arg("mount")
                .arg("-b")
                .arg(&part)
                .output();

            if let Ok(out) = status {
                if out.status.success() {
                    let msg = String::from_utf8_lossy(&out.stdout);
                    info!("Montage réussi : {}", msg.trim());
                }
            }
        }

        let disks = Disks::new_with_refreshed_list();

        // Debug additionnel : affiche la sortie de lsblk pendant l'essai
        if attempts % 3 == 0 {
            let lsblk_out = Command::new("lsblk")
                .arg("-o")
                .arg("NAME,TRAN,MOUNTPOINT")
                .output();
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
    }

    notifications::notify_backup_start(&device_config.name);

    println!(
        "{} Lancement des sauvegardes pour : {}",
        " -> ".blue(),
        device_config.name.green()
    );
    for rule in &device_config.backup_rules {
        println!(
            "   {} Synchronisation {} vers {}",
            " • ".cyan(),
            rule.source_path.yellow(),
            rule.destination_path.yellow()
        );
        // Ici : Logique de copie
    }

    notifications::notify_backup_success(&device_config.name);
}

pub fn handle_error(device_name: &str, error: &str) {
    eprintln!(
        "{} Erreur sur {} : {}",
        " ERR ".on_red(),
        device_name,
        error
    );
    notifications::notify_backup_error(device_name, error);
}

pub fn ask_user_action(vid: u16, pid: u16, product: &str) -> DeviceAction {
    println!(
        "\n{}",
        "=== Nouveau péripherique détecté ===".bright_magenta()
    );
    println!("Produit : {}", product.bright_white());
    println!("ID : {:04x}:{:04x}", vid, pid);
    println!("Que voulez-vous faire ?");
    println!("1. Whitelister (Ajouter à la config)");
    println!("2. Ignorer pour toujours");
    println!("3. Me redemander la prochaine fois");

    print!("Votre choix (1-3) : ");
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    match input.trim() {
        "1" => DeviceAction::Whitelist,
        "2" => DeviceAction::IgnoreForever,
        _ => DeviceAction::AskEachTime,
    }
}

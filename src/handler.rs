use crate::models::device::DeviceAction;
use crate::notifications;
use colored::Colorize;
use std::io::{self, Write};
use sysinfo::Disks;

pub fn trigger_backup(device_config: &crate::models::device::DeviceConfig) {
    println!(
        "{} Préparation du backup pour {}",
        " INFO ".on_blue(),
        device_config.name
    );

    // Détection des disques via sysinfo (Cross-platform)
    let disks = Disks::new_with_refreshed_list();

    let mut found_mount_point = None;

    for disk in &disks {
        let name = disk.name().to_string_lossy();
        let mount_point = disk.mount_point().to_string_lossy();
        let file_system = disk.file_system().to_string_lossy();

        println!(
            "{} Disque trouvé: {} | Mount: {} | FS: {}",
            " DEBUG ".on_bright_black(),
            name.cyan(),
            mount_point.yellow(),
            file_system.magenta()
        );

        // On cherche une correspondance
        if mount_point
            .to_lowercase()
            .contains(&device_config.name.to_lowercase())
            || name
                .to_lowercase()
                .contains(&device_config.name.to_lowercase())
        {
            found_mount_point = Some(disk.mount_point().to_path_buf());
            break;
        }
    }

    if let Some(path) = found_mount_point {
        println!("{} Disque détecté sur : {:?}", " OK ".on_green(), path);
    } else {
        println!(
            "{} Impossible de localiser le point de montage.",
            " WARN ".on_yellow()
        );
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

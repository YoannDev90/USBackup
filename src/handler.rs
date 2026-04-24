use crate::models::device::DeviceAction;
use crate::notifications;
use colored::Colorize;
use std::io::{self, Write};
use std::process::Command;

pub fn trigger_backup(device_config: &crate::models::device::DeviceConfig) {
    println!(
        "{} Préparation du backup pour {}",
        " INFO ".on_blue(),
        device_config.name
    );

    // Tentative de montage automatique via udisksctl
    // On essaie de monter les partitions potentielles (souvent sdb1, sdc1...)
    // qui correspondent à ce disque. Pour faire ça bien en Rust pur, il faudrait
    // mapper le VID/PID au block device via libudev.
    // En attendant, on utilise une approche "best effort" via shell.
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "for dev in /dev/sd[b-z][0-9]; do udisksctl mount -b $dev 2>/dev/null && break; done"
        ))
        .status();

    if let Ok(s) = status {
        if s.success() {
            println!("{} Disque monté avec succès.", " OK ".on_green());
        }
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

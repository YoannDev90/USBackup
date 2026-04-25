use crate::models::config::AppConfig;
use crate::models::device::{BackupRule, DeviceConfig};
use crate::notifications;
use crate::storage::save_config;
use dialoguer::{Confirm, FuzzySelect, Input, MultiSelect, Select};
use log::{debug, error, info, warn};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use sysinfo::Disks;
use tokio::process::Command as TokioCommand;

#[derive(Deserialize, Debug)]
struct GithubContent {
    name: String,
    download_url: Option<String>,
    #[serde(rename = "type")]
    content_type: String,
}

fn fetch_gitignore_templates(
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("USBackup-Agent")
        .build()?;

    info!("Récupération des templates .gitignore depuis GitHub...");
    let resp: Vec<GithubContent> = client
        .get("https://api.github.com/repos/github/gitignore/contents")
        .send()?
        .json()?;

    let mut templates = Vec::new();
    for item in resp {
        if item.content_type == "file" && item.name.ends_with(".gitignore") {
            if let Some(url) = item.download_url {
                let display_name = item.name.trim_end_matches(".gitignore").to_string();
                templates.push((display_name, url));
            }
        }
    }
    Ok(templates)
}

fn download_gitignore(url: &str) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("USBackup-Agent")
        .build()?;

    let content = client.get(url).send()?.text()?;
    let mut patterns = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') {
            patterns.push(line.to_string());
        }
    }
    Ok(patterns)
}

pub async fn run_wizard(
    vid: u16,
    pid: u16,
    product: &str,
    config: &mut AppConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("\n🚀 Assistant de configuration pour : {}", product);

    let device_key = format!("{:04x}:{:04x}", vid, pid);
    let name: String = Input::new()
        .with_prompt("Nom amical pour ce périphérique")
        .default(product.into())
        .interact_text()?;

    // 1. Sélectionner le point de montage de la clé pour trouver la destination
    info!("Recherche d'un point de montage pour le périphérique (timeout 30s)...");
    let mut disks = Disks::new();
    let mut mount_info = Vec::new();

    for i in 0..15 {
        // Tenter le montage actif
        let usb_parts = find_usb_partitions();
        debug!(
            "[Essai {}] Partitions USB détectées par udev: {:?}",
            i + 1,
            usb_parts
        );

        for part in usb_parts {
            let status = TokioCommand::new("udisksctl")
                .arg("mount")
                .arg("-b")
                .arg(&part)
                .output()
                .await;

            if let Ok(out) = status {
                if out.status.success() {
                    info!("Montage réussi pour {}", part);
                } else {
                    debug!(
                        "Echec montage {} : {}",
                        part,
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
            }
        }

        disks.refresh(true);

        mount_info = disks
            .iter()
            .filter(|d| {
                let mount = d.mount_point().to_string_lossy();
                debug!(
                    "Examen du disque: {} | Mount: {}",
                    d.name().to_string_lossy(),
                    mount
                );
                !mount.starts_with("/boot") && mount != "/" && mount != "/home" && !mount.is_empty()
            })
            .map(|d| {
                let name = d.name().to_string_lossy();
                let mount = d.mount_point().to_string_lossy();
                let size_gb = d.total_space() / 1024 / 1024 / 1024;
                format!("{} ({} Go) sur {}", name, size_gb, mount)
            })
            .collect();

        if !mount_info.is_empty() {
            info!("Points de montage trouvés !");
            break;
        }

        if i < 14 {
            debug!("Aucun point de montage trouvé, attente de 2s...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }

    if mount_info.is_empty() {
        return Err(
            "Aucun disque externe (clé USB) n'a été détecté. Assurez-vous qu'elle est bien montée."
                .into(),
        );
    }

    let selection = Select::new()
        .with_prompt("Quel est votre disque USB ?")
        .items(&mount_info)
        .default(0)
        .interact()?;

    let chosen_path = mount_info[selection].split(" sur ").last().unwrap();
    let usb_root = PathBuf::from(chosen_path);

    // 2. Sélectionner le dossier de destination SUR la clé
    let dest_folder: String = Input::new()
        .with_prompt("Dossier de destination sur la clé (ex: backups/mon_pc)")
        .default("backups/default".into())
        .interact_text()?;

    let full_dest = usb_root.join(dest_folder.trim_start_matches('/'));

    // 3. Sélectionner les dossiers sources à sauvegarder (UI simplifiée)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let common_dirs = vec![
        "Documents",
        "Images",
        "Bureau",
        "Musique",
        "Vidéos",
        "Projets",
    ];
    let mut available_sources = Vec::new();

    for dir in common_dirs {
        let path = Path::new(&home).join(dir);
        if path.exists() {
            available_sources.push(path.to_string_lossy().to_string());
        }
    }

    let chosen_indices = MultiSelect::new()
        .with_prompt("Quels dossiers voulez-vous cloner automatiquement ? (Espace pour cocher)")
        .items(&available_sources)
        .interact()?;

    // 4. Définir des exclusions
    let mut rules = Vec::new();
    let setup_exclusions = Confirm::new()
        .with_prompt("Voulez-vous configurer des exclusions via templates .gitignore (GitHub) ?")
        .interact()?;

    let mut exclusions = Vec::new();
    if setup_exclusions {
        let templates_res = tokio::task::spawn_blocking(fetch_gitignore_templates).await;

        if let Ok(Ok(templates)) = templates_res {
            let template_names: Vec<_> = templates.iter().map(|(n, _)| n.as_str()).collect();

            loop {
                let selection = FuzzySelect::new()
                    .with_prompt("Sélectionnez un template .gitignore (Echap/Vide pour terminer)")
                    .items(&template_names)
                    .interact_opt()?;

                if let Some(idx) = selection {
                    info!("Téléchargement du template {}...", templates[idx].0);
                    let url = templates[idx].1.clone();
                    let download_res =
                        tokio::task::spawn_blocking(move || download_gitignore(&url)).await;

                    match download_res {
                        Ok(Ok(mut patterns)) => {
                            info!(
                                "{} patterns ajoutés depuis {}",
                                patterns.len(),
                                templates[idx].0
                            );
                            exclusions.append(&mut patterns);
                        }
                        _ => error!("Erreur lors du téléchargement."),
                    }
                    if !Confirm::new()
                        .with_prompt("Ajouter un autre template ?")
                        .interact()?
                    {
                        break;
                    }
                } else {
                    break;
                }
            }
        } else {
            warn!(
                "Impossible de récupérer les templates depuis GitHub. Passage en saisie manuelle."
            );
        }

        println!("\nSyntaxe .gitignore acceptée (ex: node_modules/, *.log, temp/**). Laissez vide pour terminer.");
        loop {
            let pattern: String = Input::new()
                .with_prompt("Mot-clé personnalisé ou pattern (.gitignore style)")
                .allow_empty(true)
                .interact_text()?;

            if pattern.is_empty() {
                break;
            }
            exclusions.push(pattern);
        }
    }

    let delete_missing = Confirm::new()
        .with_prompt("Voulez-vous supprimer les fichiers sur la clé s'ils sont supprimés de la source (miroir exact) ?")
        .default(true)
        .interact()?;

    for idx in chosen_indices {
        rules.push(BackupRule {
            source_path: available_sources[idx].clone(),
            destination_path: full_dest.to_string_lossy().to_string(),
            exclude: exclusions.clone(),
            delete_missing,
        });
    }

    let new_device = DeviceConfig {
        name: name.clone(),
        vendor_id: vid,
        product_id: pid,
        action: crate::models::device::DeviceAction::Whitelist,
        backup_rules: rules,
    };

    config.devices.insert(device_key, new_device);
    save_config(config)?;

    println!("\n✅ Configuration enregistrée pour {} !", name);
    Ok(())
}

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

    if let Some(usb_path) = found_mount_point {
        info!("Disque détecté sur : {:?}", usb_path);

        notifications::notify_backup_start(&device_config.name);

        for rule in &device_config.backup_rules {
            let full_dest = usb_path.join(rule.destination_path.trim_start_matches('/'));

            info!("Synchronisation {} vers {:?}", rule.source_path, full_dest);

            // Vérifier si la source existe
            if !Path::new(&rule.source_path).exists() {
                error!("Source inexistante : {}", rule.source_path);
                continue;
            }

            // Créer le répertoire de destination s'il n'existe pas
            if let Err(e) = std::fs::create_dir_all(&full_dest) {
                error!("Impossible de créer la destination {:?} : {}", full_dest, e);
                continue;
            }

            // Utilisation de rsync pour une synchronisation efficace
            let mut cmd = TokioCommand::new("rsync");
            cmd.arg("-avz");

            if rule.delete_missing {
                cmd.arg("--delete");
            }

            // Ajout des exclusions
            for pattern in &rule.exclude {
                cmd.arg(format!("--exclude={}", pattern));
            }

            let status = cmd.arg(&rule.source_path).arg(&full_dest).status().await;

            match status {
                Ok(s) if s.success() => info!("Succès pour {}", rule.source_path),
                Ok(s) => error!(
                    "Rsync a échoué avec le code {} pour {}",
                    s, rule.source_path
                ),
                Err(e) => error!("Erreur lors de l'exécution de rsync : {}", e),
            }
        }
    } else {
        warn!("Impossible de localiser le point de montage.");
        return;
    }

    notifications::notify_backup_success(&device_config.name);
}

// Code pour ask_user_action supprimé car incompatible avec le mode TUI actuel.

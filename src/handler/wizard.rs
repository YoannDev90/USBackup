use crate::handler::github::{download_gitignore, fetch_gitignore_templates};
use crate::handler::udev_utils::{find_usb_partitions, mount_partition};
use crate::models::config::AppConfig;
use crate::models::device::{BackupRule, DeviceAction, DeviceConfig};
use crate::storage::save_config;
use dialoguer::{Confirm, FuzzySelect, Input, MultiSelect, Select};
use log::{debug, error, info, warn};
use std::path::{Path, PathBuf};
use sysinfo::Disks;

pub async fn run_wizard(
    vid: u16,
    pid: u16,
    product: &str,
    uuid: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("\n🚀 Configuration assistant for: {}", product);

    let name: String = Input::new()
        .with_prompt("Friendly name for this device")
        .default(product.into())
        .interact_text()?;

    // 1. Select the mount point of the key to find the destination
    info!("Searching for a mount point for the device (timeout 30s)...");
    let mut disks = Disks::new();
    let mut mount_info = Vec::new();
    let mut chosen_usb_root = None;

    for i in 0..15 {
        // Attempt active mount
        let usb_parts = find_usb_partitions();
        for part in usb_parts {
            if let Some(u) = crate::handler::udev_utils::get_partition_uuid(&part) {
                if u == uuid {
                    if mount_partition(&part).await {
                        info!("Mount successful for {}", part);
                    }
                }
            }
        }

        disks.refresh(true);

        mount_info = disks
            .iter()
            .filter(|d| {
                let mount = d.mount_point().to_string_lossy();
                // On cherche spécifiquement la clé avec notre UUID
                let usb_parts = find_usb_partitions();
                let mut matches_uuid = false;
                for part in usb_parts {
                    if let Some(u) = crate::handler::udev_utils::get_partition_uuid(&part) {
                        if u == uuid {
                            // On ne peut pas facilement lier disk à part ici sans plus de logique,
                            // on garde une heuristique ou on check toutes les parts
                            matches_uuid = true;
                        }
                    }
                }
                matches_uuid
                    && !mount.starts_with("/boot")
                    && mount != "/"
                    && mount != "/home"
                    && !mount.is_empty()
            })
            .map(|d| {
                let name = d.name().to_string_lossy();
                let mount = d.mount_point().to_string_lossy();
                let size_gb = d.total_space() / 1024 / 1024 / 1024;
                format!("{} ({} GB) on {}", name, size_gb, mount)
            })
            .collect();

        if !mount_info.is_empty() {
            info!("Mount points found!");
            break;
        }

        if i < 14 {
            debug!("No mount point found, waiting 2s...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }

    if mount_info.is_empty() {
        error!("No external disk (USB key) detected with UUID {}. Please make sure it is properly mounted.", uuid);
        return Ok(());
    }

    let selection = Select::new()
        .with_prompt("Which one is your USB disk?")
        .items(&mount_info)
        .default(0)
        .interact()?;

    let selected_mount_info = &mount_info[selection];
    let chosen_path = selected_mount_info.split(" on ").last().unwrap();
    let usb_root = PathBuf::from(chosen_path);
    chosen_usb_root = Some(usb_root.clone());

    // 2. Select the destination folder ON the key
    let dest_folder: String = Input::new()
        .with_prompt("Destination folder on the key (e.g., backups/my_pc)")
        .default("backups/default".into())
        .interact_text()?;

    let full_dest = usb_root.join(dest_folder.trim_start_matches('/'));

    // 3. Select source folders to backup (simplified UI)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let common_dirs = vec![
        "Documents",
        "Pictures",
        "Desktop",
        "Music",
        "Videos",
        "Projects",
    ];
    let mut available_sources = Vec::new();

    for dir in common_dirs {
        let path = Path::new(&home).join(dir);
        if path.exists() {
            available_sources.push(path.to_string_lossy().to_string());
        }
    }

    let chosen_indices = MultiSelect::new()
        .with_prompt("Which folders do you want to clone automatically? (Space to check)")
        .items(&available_sources)
        .interact()?;

    // 4. Set exclusions
    let mut rules = Vec::new();
    let setup_exclusions = Confirm::new()
        .with_prompt("Do you want to configure exclusions via .gitignore templates (GitHub)?")
        .interact()?;

    // Load default exclusions packaged in the binary
    let default_excludes_str = include_str!("../exclude_list.txt");
    let mut exclusions: Vec<String> = default_excludes_str
        .lines()
        .map(|l: &str| l.trim())
        .filter(|l: &&str| !l.is_empty() && !l.starts_with("//"))
        .map(|l: &str| l.to_string())
        .collect();

    if setup_exclusions {
        let templates_res = tokio::task::spawn_blocking(fetch_gitignore_templates).await;

        if let Ok(Ok(templates)) = templates_res {
            let template_names: Vec<_> = templates
                .iter()
                .map(|(n, _): &(String, String)| n.as_str())
                .collect();

            loop {
                let selection = FuzzySelect::new()
                    .with_prompt("Select a .gitignore template (Esc/Empty to finish)")
                    .items(&template_names)
                    .interact_opt()?;

                if let Some(idx) = selection {
                    info!("Downloading template {}...", templates[idx].0);
                    let url = templates[idx].1.clone();
                    let download_res =
                        tokio::task::spawn_blocking(move || download_gitignore(&url)).await;

                    match download_res {
                        Ok(Ok(mut patterns)) => {
                            info!(
                                "{} patterns added from {}",
                                patterns.len(),
                                templates[idx].0
                            );
                            exclusions.append(&mut patterns);
                        }
                        _ => error!("Error during download."),
                    }
                    if !Confirm::new()
                        .with_prompt("Add another template?")
                        .interact()?
                    {
                        break;
                    }
                } else {
                    break;
                }
            }
        } else {
            warn!("Unable to fetch templates from GitHub. Falling back to manual entry.");
        }

        println!("\nAccepted .gitignore syntax (e.g., node_modules/, *.log, temp/**). Leave empty to finish.");
        loop {
            let pattern: String = Input::new()
                .with_prompt("Custom keyword or pattern (.gitignore style)")
                .allow_empty(true)
                .interact_text()?;

            if pattern.is_empty() {
                break;
            }
            exclusions.push(pattern);
        }
    }

    let delete_missing = Confirm::new()
        .with_prompt("Do you want to delete files on the key if they are deleted from the source (exact mirror)?")
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
        uuid: Some(uuid.to_string()),
        action: DeviceAction::Whitelist,
        backup_rules: rules,
    };

    if let Some(usb_root) = chosen_usb_root {
        crate::storage::save_device_config(&usb_root, &new_device)?;
        println!(
            "\n✅ Configuration registered locally on the key ({:?})!",
            usb_root.join(".usbackup.toml")
        );
    }

    Ok(())
}

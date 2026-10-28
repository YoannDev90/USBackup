use crate::handler::github::{download_gitignore, fetch_gitignore_templates};
use crate::device::{is_removable_mount, mount_partition, to_devnode};
use crate::models::device::{BackupRule, DeviceAction, DeviceConfig};
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
    // (display label, mount path) — keep the path structured so we never
    // have to re-parse a formatted string (fragile if names contain " on ").
    let mut mount_choices: Vec<(String, PathBuf)> = Vec::new();

    for i in 0..15 {
        // Attempt active mount for the partition matching our UUID + VID/PID
        if let Some(part) =
            crate::device::find_partition_by_uuid(uuid, Some(vid), Some(pid))
        {
            if mount_partition(&part.devnode).await {
                info!("Mount successful for {}", part.devnode);
            }
        }

        disks.refresh(true);

        mount_choices = disks
            .iter()
            .filter(|d| {
                let mount = d.mount_point().to_string_lossy();
                if mount.starts_with("/boot") || mount == "/" || mount == "/home" || mount.is_empty()
                {
                    return false;
                }
                // Prefer a strict match: disk backing devnode is our USB partition
                let disk_devnode = to_devnode(&d.name().to_string_lossy());
                let strict = crate::device::find_usb_partitions().iter().any(|p| {
                    p.uuid.as_deref() == Some(uuid) && p.devnode == disk_devnode
                });
                if strict {
                    return true;
                }
                // Fallback: any USB partition with our UUID exists and this disk
                // sits under a removable media mount point
                let uuid_present = crate::device::find_usb_partitions()
                    .iter()
                    .any(|p| p.uuid.as_deref() == Some(uuid));
                uuid_present && is_removable_mount(&mount)
            })
            .map(|d| {
                let name = d.name().to_string_lossy();
                let mount = d.mount_point().to_path_buf();
                let size_gb = d.total_space() / 1024 / 1024 / 1024;
                let label = format!("{} ({} GB) on {}", name, size_gb, mount.display());
                (label, mount)
            })
            .collect();

        if !mount_choices.is_empty() {
            info!("Mount points found!");
            break;
        }

        if i < 14 {
            debug!("No mount point found, waiting 2s...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }

    if mount_choices.is_empty() {
        error!("No external disk (USB key) detected with UUID {}. Please make sure it is properly mounted.", uuid);
        return Ok(());
    }

    let labels: Vec<&str> = mount_choices.iter().map(|(label, _)| label.as_str()).collect();
    let selection = Select::new()
        .with_prompt("Which one is your USB disk?")
        .items(&labels)
        .default(0)
        .interact()?;

    let usb_root = mount_choices[selection].1.clone();

    // 2. Select the destination folder ON the key
    let dest_folder: String = Input::new()
        .with_prompt("Relative destination folder on the key (e.g., backups/my_pc)")
        .default("backups/default".into())
        .interact_text()?;

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

    let incremental = Confirm::new()
        .with_prompt("Enable incremental snapshots (versioning)?")
        .default(false)
        .interact()?;

    let unmount_after = Confirm::new()
        .with_prompt("Automatically unmount the key after backup?")
        .default(true)
        .interact()?;

    let post_script: String = Input::new()
        .with_prompt("Optional post-backup shell command (leave empty for none)")
        .allow_empty(true)
        .interact_text()?;

    let post_backup_script = if post_script.is_empty() {
        None
    } else {
        Some(post_script)
    };

    for idx in chosen_indices {
        rules.push(BackupRule {
            source_path: available_sources[idx].clone(),
            destination_path: dest_folder.clone(),
            exclude: exclusions.clone(),
            delete_missing,
            incremental,
            post_backup_script: post_backup_script.clone(),
            unmount_after,
        });
    }

    let mut new_device = DeviceConfig {
        name: name.clone(),
        vendor_id: vid,
        product_id: pid,
        uuid: Some(uuid.to_string()),
        signature: None,
        action: DeviceAction::Whitelist,
        backup_rules: rules,
    };

    // Sign the configuration with the local secret key
    let config_main = crate::storage::load_config();
    crate::storage::sign_config(&mut new_device, &config_main.secret_key);

    crate::storage::save_device_config(&usb_root, &new_device)?;
    println!(
        "\n✅ Configuration registered locally on the key ({:?})!",
        usb_root.join(".usbackup.toml")
    );

    Ok(())
}

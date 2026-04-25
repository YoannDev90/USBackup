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

    info!("Fetching .gitignore templates from GitHub...");
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
    println!("\n🚀 Configuration assistant for: {}", product);

    let device_key = format!("{:04x}:{:04x}", vid, pid);
    let name: String = Input::new()
        .with_prompt("Friendly name for this device")
        .default(product.into())
        .interact_text()?;

    // 1. Select the mount point of the key to find the destination
    info!("Searching for a mount point for the device (timeout 30s)...");
    let mut disks = Disks::new();
    let mut mount_info = Vec::new();

    for i in 0..15 {
        // Attempt active mount
        let usb_parts = find_usb_partitions();
        debug!(
            "[Attempt {}] USB partitions detected by udev: {:?}",
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
                    info!("Mount successful for {}", part);
                } else {
                    debug!(
                        "Mount failed for {} : {}",
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
                    "Examining disk: {} | Mount: {}",
                    d.name().to_string_lossy(),
                    mount
                );
                !mount.starts_with("/boot") && mount != "/" && mount != "/home" && !mount.is_empty()
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
        return Err(
            "No external disk (USB key) detected. Please make sure it is properly mounted.".into(),
        );
    }

    let selection = Select::new()
        .with_prompt("Which one is your USB disk?")
        .items(&mount_info)
        .default(0)
        .interact()?;

    let chosen_path = mount_info[selection].split(" on ").last().unwrap();
    let usb_root = PathBuf::from(chosen_path);

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
    let default_excludes_str = include_str!("exclude_list.txt");
    let mut exclusions: Vec<String> = default_excludes_str
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(|l| l.to_string())
        .collect();

    if setup_exclusions {
        let templates_res = tokio::task::spawn_blocking(fetch_gitignore_templates).await;

        if let Ok(Ok(templates)) = templates_res {
            let template_names: Vec<_> = templates.iter().map(|(n, _)| n.as_str()).collect();

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
                debug!("udev examining: {:?}", devnode);
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
                    debug!("USB Partition found via udev: {:?}", devnode);
                    partitions.push(devnode.to_string_lossy().to_string());
                }
            }
        }
    }
    partitions
}

pub async fn trigger_backup(device_config: &crate::models::device::DeviceConfig) {
    info!("Preparing backup for {}", device_config.name);

    // Disk detection via sysinfo (Cross-platform) with retries
    let mut found_mount_point = None;
    let mut attempts = 0;

    debug!("Monitoring mount (30 attempts)...");

    while attempts < 30 && found_mount_point.is_none() {
        if attempts > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }

        // ATTEMPT MOUNT ON EVERY TRY if not mounted
        let usb_parts = find_usb_partitions();
        for part in usb_parts {
            // Attempt to mount all found USB partitions via udisksctl (Linux)
            let status = TokioCommand::new("udisksctl")
                .arg("mount")
                .arg("-b")
                .arg(&part)
                .output()
                .await;

            if let Ok(out) = status {
                if out.status.success() {
                    let msg = String::from_utf8_lossy(&out.stdout);
                    info!("Mount successful: {}", msg.trim());
                }
            }
        }

        let disks = Disks::new_with_refreshed_list();

        // Additional debug: show lsblk output during the attempt
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
                "[Attempt {}] Disk found: {} | Mount: {} | FS: {}",
                attempts + 1,
                name,
                mount_point,
                file_system
            );

            // Improved detection strategy
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
        info!("Disk detected on: {:?}", usb_path);

        notifications::notify_backup_start(&device_config.name);

        for rule in &device_config.backup_rules {
            let full_dest = usb_path.join(rule.destination_path.trim_start_matches('/'));

            info!("Syncing {} to {:?}", rule.source_path, full_dest);

            // Check if source exists
            if !Path::new(&rule.source_path).exists() {
                error!("Source missing: {}", rule.source_path);
                continue;
            }

            // Create destination directory if it doesn't exist
            if let Err(e) = std::fs::create_dir_all(&full_dest) {
                error!("Unable to create destination {:?}: {}", full_dest, e);
                continue;
            }

            // Use rsync for efficient synchronization
            let mut cmd = TokioCommand::new("rsync");
            cmd.arg("-avz");

            if rule.delete_missing {
                cmd.arg("--delete");
            }

            // Add exclusions
            for pattern in &rule.exclude {
                cmd.arg(format!("--exclude={}", pattern));
            }

            let status = cmd.arg(&rule.source_path).arg(&full_dest).status().await;

            match status {
                Ok(s) if s.success() => info!("Success for {}", rule.source_path),
                Ok(s) => error!("Rsync failed with code {} for {}", s, rule.source_path),
                Err(e) => error!("Error during rsync execution: {}", e),
            }
        }
    } else {
        warn!("Unable to locate mount point.");
        return;
    }

    notifications::notify_backup_success(&device_config.name);
}

// Code for ask_user_action removed as incompatible with the current TUI mode.

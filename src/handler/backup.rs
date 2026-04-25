use crate::handler::udev_utils::{find_usb_partitions, mount_partition};
use crate::models::device::DeviceConfig;
use crate::notifications;
use log::{debug, error, info, warn};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use sysinfo::Disks;
use tokio::process::Command as TokioCommand;

pub async fn trigger_backup(device_config: &DeviceConfig) {
    inner_trigger_backup(device_config, None).await;
}

pub async fn trigger_backup_by_uuid(vid: u16, pid: u16, uuid: &str) -> Option<DeviceConfig> {
    info!(
        "Waiting for device {} to be mounted to load local config...",
        uuid
    );

    // Heureusement, la boucle de détection dans inner_trigger_backup gère déjà l'attente
    // Mais ici on n'a pas encore la DeviceConfig.
    // On va faire une boucle d'attente simplifiée ici pour trouver le point de montage

    let mut found_mount_point = None;
    let mut attempts = 0;
    while attempts < 30 && found_mount_point.is_none() {
        if attempts > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }

        let usb_parts = find_usb_partitions();
        for part in usb_parts {
            if let Some(u) = crate::handler::udev_utils::get_partition_uuid(&part) {
                if u == uuid {
                    if mount_partition(&part).await {
                        // Succès
                    }
                    // Trouver où c'est monté
                    let disks = Disks::new_with_refreshed_list();
                    if let Some(d) = disks.iter().find(|d| {
                        // Heuristique simplifiée pour l'instant
                        crate::handler::udev_utils::get_partition_uuid(&part).as_deref()
                            == Some(uuid)
                    }) {
                        found_mount_point = Some(d.mount_point().to_path_buf());
                        break;
                    }
                }
            }
        }
        attempts += 1;
    }

    if let Some(mount_point) = found_mount_point {
        if let Some(conf) = crate::storage::load_device_config(&mount_point) {
            info!("Local config loaded from {:?}", mount_point);
            inner_trigger_backup(&conf, Some(mount_point)).await;
            return Some(conf);
        } else {
            error!(
                "Appareil approuvé mais aucun fichier .usbackup.toml trouvé sur {:?}",
                mount_point
            );
        }
    } else {
        error!("Impossible de monter l'appareil {} après 30s.", uuid);
    }
    None
}

async fn inner_trigger_backup(
    device_config: &DeviceConfig,
    pre_found_mount: Option<std::path::PathBuf>,
) {
    info!("Preparing backup for {}", device_config.name);

    // Disk detection via sysinfo (Cross-platform) with retries
    let mut found_mount_point = pre_found_mount;
    let mut attempts = 0;

    if found_mount_point.is_none() {
        debug!("Monitoring mount (30 attempts)...");
        while attempts < 30 && found_mount_point.is_none() {
            if attempts > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            }

            // ATTEMPT MOUNT ON EVERY TRY if not mounted
            let usb_parts = find_usb_partitions();
            for part in usb_parts {
                if mount_partition(&part).await {
                    info!("Mount successful for {}", part);
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

                // Detection strategy: UUID first, then name heuristics
                let mut matches = false;

                // Try matching by UUID if available
                if let Some(conf_uuid) = &device_config.uuid {
                    // We need to find the devnode for this disk to get its UUID
                    // sysinfo doesn't give us the UUID directly easily,
                    // so we check partitions found by udev
                    let usb_parts = find_usb_partitions();
                    for part in usb_parts {
                        if let Some(u) = crate::handler::udev_utils::get_partition_uuid(&part) {
                            if u == *conf_uuid {
                                // Verify this partition is indeed mounted at mount_point
                                if disk.mount_point().to_string_lossy() == mount_point {
                                    matches = true;
                                    break;
                                }
                            }
                        }
                    }
                }

                if !matches {
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
                        matches = true;
                    }
                }

                if matches {
                    found_mount_point = Some(disk.mount_point().to_path_buf());
                    break;
                }
            }
            attempts += 1;
        }
    }

    if let Some(usb_path) = found_mount_point {
        info!("Disk detected on: {:?}", usb_path);

        notifications::notify_backup_start(&device_config.name);

        let mut has_errors = false;
        for rule in &device_config.backup_rules {
            let full_dest = usb_path.join(rule.destination_path.trim_start_matches('/'));

            info!("Syncing {} to {:?}", rule.source_path, full_dest);

            // Check if source exists
            if !Path::new(&rule.source_path).exists() {
                error!("Source missing: {}", rule.source_path);
                has_errors = true;
                continue;
            }

            // Create destination directory if it doesn't exist
            if let Err(e) = std::fs::create_dir_all(&full_dest) {
                error!("Unable to create destination {:?}: {}", full_dest, e);
                has_errors = true;
                continue;
            }

            // Use rsync for efficient synchronization
            let mut cmd = TokioCommand::new("rsync");
            cmd.arg("-avz").arg("--progress");

            if rule.delete_missing {
                cmd.arg("--delete");
            }

            // Add exclusions
            for pattern in &rule.exclude {
                cmd.arg(format!("--exclude={}", pattern));
            }

            // Automatic .gitignore detection
            if let Ok(gitignore_path) = find_local_gitignore(&rule.source_path) {
                info!(
                    "Using local .gitignore exclusions from {:?}",
                    gitignore_path
                );
                cmd.arg(format!(
                    "--exclude-from={}",
                    gitignore_path.to_string_lossy()
                ));
            }

            let output = cmd.arg(&rule.source_path).arg(&full_dest).output().await;

            match output {
                Ok(out) if out.status.success() => {
                    info!("Success for {}", rule.source_path);
                    append_to_log(
                        &device_config.name,
                        &rule.source_path,
                        &String::from_utf8_lossy(&out.stdout),
                    );
                }
                Ok(out) => {
                    let code = out
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "Unknown".to_string());
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    error!("Rsync failed with code {} for {}", code, rule.source_path);
                    append_to_log(
                        &device_config.name,
                        &rule.source_path,
                        &format!("FAILED (code {})\n{}", code, stderr),
                    );
                    has_errors = true;
                }
                Err(e) => {
                    error!("Error during rsync execution: {}", e);
                    append_to_log(
                        &device_config.name,
                        &rule.source_path,
                        &format!("EXECUTION ERROR: {}", e),
                    );
                    has_errors = true;
                }
            }
        }

        if has_errors {
            notifications::notify_backup_error(
                &device_config.name,
                "Certaines synchronisations ont échoué. Vérifiez les logs.",
            );
        } else {
            notifications::notify_backup_success(&device_config.name);
        }
    } else {
        warn!("Unable to locate mount point.");
        notifications::notify_backup_error(
            &device_config.name,
            "Impossible de localiser le point de montage du disque.",
        );
        return;
    }
}

fn append_to_log(device_name: &str, source: &str, content: &str) {
    let log_dir = dirs::home_dir()
        .map(|h| h.join(".local/share/usbackup/logs"))
        .unwrap_or_else(|| Path::new("/tmp/usbackup/logs").to_path_buf());

    info!("Logging to: {:?}", log_dir);

    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        error!("Could not create log directory: {}", e);
        return;
    }

    let log_file = log_dir.join(format!("{}.log", device_name.replace(' ', "_")));
    info!("Writing to file: {:?}", log_file);
    let mut file = match OpenOptions::new().create(true).append(true).open(&log_file) {
        Ok(f) => f,
        Err(e) => {
            error!("Could not open log file {:?}: {}", log_file, e);
            return;
        }
    };

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let log_entry = format!(
        "\n--- [{}] Source: {} ---\n{}\n---------------------------\n",
        timestamp, source, content
    );

    if let Err(e) = file.write_all(log_entry.as_bytes()) {
        error!("Could not write to log file: {}", e);
    }
}

fn find_local_gitignore(source_path: &str) -> Result<std::path::PathBuf, std::io::Error> {
    let source = Path::new(source_path);
    let gitignore = source.join(".gitignore");
    if gitignore.exists() {
        return Ok(gitignore);
    }
    // Check parent if source is a file/subdir but part of a git repo
    if let Some(parent) = source.parent() {
        let parent_gitignore = parent.join(".gitignore");
        if parent_gitignore.exists() {
            return Ok(parent_gitignore);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "No .gitignore found",
    ))
}

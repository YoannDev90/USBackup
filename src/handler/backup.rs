use crate::handler::udev_utils::{find_usb_partitions, mount_partition};
use crate::models::device::DeviceConfig;
use crate::notifications;
use chrono;
use directories::ProjectDirs;
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, error, info, warn};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use sysinfo::Disks;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;

// Laisse cette fonction publique si tu prévois de l'appeler ailleurs,
// sinon on peut la marquer comme autorisée ou la supprimer.
#[allow(dead_code)]
pub async fn trigger_backup(device_config: &DeviceConfig) {
    inner_trigger_backup(device_config, None).await;
}

pub async fn trigger_backup_by_uuid(_vid: u16, _pid: u16, uuid: &str) -> Option<DeviceConfig> {
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
                        let mount = d.mount_point().to_string_lossy();
                        // Filtrage strict : ignorer les partitions système et home
                        if mount == "/"
                            || mount.starts_with("/boot")
                            || mount.starts_with("/home")
                            || mount.starts_with("/run/user")
                        {
                            return false;
                        }

                        // Si c'est monté dans /run/media ou /media, c'est probablement notre clé
                        if mount.starts_with("/run/media/") || mount.starts_with("/media/") {
                            return true;
                        }

                        // Sinon, on vérifie l'UUID
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
            let main_config = crate::storage::load_config();
            if crate::storage::verify_signature(&conf, &main_config.secret_key) {
                info!("Local config loaded and verified from {:?}", mount_point);
                inner_trigger_backup(&conf, Some(mount_point)).await;
                return Some(conf);
            } else {
                error!("SECURITY ALERT: Invalid signature for local config on {:?}. The config may have been tampered with or created on another machine.", mount_point);
                notifications::notify_backup_error(
                    &conf.name,
                    "Signature de configuration invalide ! Backup annulée par sécurité.",
                );
            }
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
            // Sécurité : Empêcher l'utilisation de chemins absolus ou de traversal pour la destination
            if rule.destination_path.starts_with('/') {
                warn!(
                    "Destination path '{}' is absolute. Forcing relative to USB root.",
                    rule.destination_path
                );
            }

            let sanitized_dest = rule
                .destination_path
                .trim_start_matches('/')
                .replace("..", "__"); // Protection basique contre le path traversal

            let full_dest = usb_path.join(sanitized_dest);

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
            // -a: archive, -v: verbose, -z: compress during transfer, -P: progress and partial
            cmd.arg("-avzP");

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

            // Incremental snapshots handling
            let final_dest = if rule.incremental {
                let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
                let snapshot_path = full_dest.join(&timestamp);
                let latest_link = full_dest.join("latest");

                if latest_link.exists() {
                    cmd.arg(format!("--link-dest={}", latest_link.to_string_lossy()));
                }

                if let Err(e) = std::fs::create_dir_all(&snapshot_path) {
                    error!(
                        "Unable to create snapshot directory {:?}: {}",
                        snapshot_path, e
                    );
                    has_errors = true;
                    continue;
                }
                snapshot_path
            } else {
                full_dest.clone()
            };

            cmd.arg(&rule.source_path).arg(&final_dest);
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to spawn rsync: {}", e);
                    has_errors = true;
                    continue;
                }
            };

            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let mut stdout_reader = BufReader::new(stdout).lines();
            let mut stderr_reader = BufReader::new(stderr).lines();

            let pb = ProgressBar::new(100);
            pb.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {msg} ({pos}%)",
                )
                .unwrap()
                .progress_chars("#>-"),
            );
            pb.set_message(format!(
                "Syncing {}",
                Path::new(&rule.source_path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));

            let mut full_output = String::new();
            let mut error_output = String::new();

            loop {
                tokio::select! {
                    line = stdout_reader.next_line() => {
                        match line {
                            Ok(Some(l)) => {
                                // Rsync --progress outputs lines like:
                                //       32,768   0%    0.00kB/s    0:00:00
                                if l.contains('%') {
                                    let parts: Vec<&str> = l.split_whitespace().collect();
                                    for part in parts {
                                        if part.contains('%') {
                                            let pct_str = part.trim_end_matches('%');
                                            if let Ok(pct) = pct_str.parse::<u64>() {
                                                pb.set_position(pct);
                                            }
                                        }
                                    }
                                }
                                full_output.push_str(&l);
                                full_output.push('\n');
                            }
                            _ => break,
                        }
                    }
                    line = stderr_reader.next_line() => {
                        if let Ok(Some(l)) = line {
                            error_output.push_str(&l);
                            error_output.push('\n');
                        }
                    }
                }
            }

            let status = child.wait().await;
            pb.finish_and_clear();

            match status {
                Ok(s) if s.success() => {
                    info!("Success for {}", rule.source_path);
                    append_to_log(&device_config.name, &rule.source_path, &full_output);

                    // Update 'latest' symlink for incremental backups
                    if rule.incremental {
                        let latest_link = full_dest.join("latest");
                        if latest_link.exists() {
                            let _ = std::fs::remove_file(&latest_link);
                        }
                        // Need the relative path for the symlink to be portable
                        if let Some(snapshot_name) = final_dest.file_name() {
                            let _ = std::os::unix::fs::symlink(snapshot_name, &latest_link);
                        }
                    }

                    // Post-backup script
                    if let Some(script) = &rule.post_backup_script {
                        info!("Running post-backup script: {}", script);
                        let _ = TokioCommand::new("sh").arg("-c").arg(script).spawn();
                    }
                }
                Ok(s) => {
                    let code = s.code().unwrap_or(-1);
                    error!("Rsync failed with code {} for {}", code, rule.source_path);
                    notifications::notify_backup_error(
                        &device_config.name,
                        &format!("Erreur rsync (code {}). Consultez les logs.", code),
                    );
                    append_to_log(
                        &device_config.name,
                        &rule.source_path,
                        &format!("FAILED (code {})\n{}", code, error_output),
                    );
                    has_errors = true;
                }
                Err(e) => {
                    error!("Error waiting for rsync: {}", e);
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

        // Global Post-backup actions for the device
        let should_unmount = device_config.backup_rules.iter().any(|r| r.unmount_after);

        if should_unmount {
            info!("Unmounting device {:?}", usb_path);
            let _ = TokioCommand::new("udisksctl")
                .arg("unmount")
                .arg("-b")
                .arg(usb_path.to_string_lossy().to_string()) // This might need dev node instead of mount point
                .output()
                .await;
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

fn get_log_path(device_name: &str) -> PathBuf {
    if let Some(proj_dirs) = ProjectDirs::from("com", "usbackup", "USBackup") {
        let log_dir = proj_dirs.data_dir().join("logs");
        if !log_dir.exists() {
            let _ = std::fs::create_dir_all(&log_dir);
        }
        return log_dir.join(format!("{}.log", device_name.replace(' ', "_")));
    }
    PathBuf::from(format!("{}.log", device_name.replace(' ', "_")))
}

fn append_to_log(device_name: &str, source: &str, content: &str) {
    let log_file = get_log_path(device_name);
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

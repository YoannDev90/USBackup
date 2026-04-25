use log::debug;
use tokio::process::Command as TokioCommand;

pub fn find_usb_partitions() -> Vec<String> {
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

pub fn get_partition_uuid(devnode: &str) -> Option<String> {
    let mut enumerator = udev::Enumerator::new().ok()?;
    enumerator.match_subsystem("block").ok()?;

    let devices = enumerator.scan_devices().ok()?;
    for device in devices {
        if let Some(node) = device.devnode() {
            if node.to_string_lossy() == devnode {
                return device
                    .property_value("ID_FS_UUID")
                    .map(|v| v.to_string_lossy().to_string());
            }
        }
    }
    None
}

pub async fn mount_partition(part: &str) -> bool {
    let output = TokioCommand::new("udisksctl")
        .arg("mount")
        .arg("-b")
        .arg(part)
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            debug!("Successfully mounted partition {}", part);
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // On ne log en error que si ce n'est pas "already mounted" car c'est un cas fréquent et normal
            if stderr.contains("already mounted") {
                debug!("Partition {} is already mounted.", part);
                true
            } else {
                debug!("udisksctl mount failed for {}: {}", part, stderr);
                false
            }
        }
        Err(e) => {
            debug!("Failed to execute udisksctl: {}", e);
            false
        }
    }
}

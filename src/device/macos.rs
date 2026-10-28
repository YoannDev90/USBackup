//! macOS device backend: `diskutil` for enumeration/mount/unmount and
//! `system_profiler` for the USB vendor/product ids.
//!
//! `diskutil list -plist external physical` filters external physical disks
//! natively — the counterpart of udev's `ID_BUS=usb` — and reports
//! `DeviceIdentifier`, `VolumeUUID` and `MountPoint` per volume. VID/PID come
//! from `system_profiler SPUSBDataType -json` keyed by the BSD disk name
//! (`media[].bsd_name`), the ids sitting on the parent USB device node
//! (plan_mac.md M2).
use super::UsbPartition;
use log::debug;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::process::Command;
use tokio::process::Command as TokioCommand;

/// One volume entry from `diskutil list -plist`.
struct Volume {
    /// BSD device identifier without the `/dev/` prefix (`disk2s1`).
    device: String,
    uuid: Option<String>,
    mount_point: Option<String>,
}

/// `system_profiler` reports ids as hex strings (`0x0781`).
fn parse_hex_u16(s: &str) -> Option<u16> {
    u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// Read one string field from a `diskutil -plist` payload.
fn plist_string(xml: &[u8], key: &str) -> Option<String> {
    let value = plist::Value::from_reader(Cursor::new(xml)).ok()?;
    let dict = value.as_dictionary()?;
    dict.get(key)
        .and_then(|v| v.as_string())
        .map(str::to_string)
}

/// Parse `diskutil list -plist` into volumes. A partitioned disk lists its
/// `Partitions`; a plain key without a partition table carries its volume keys
/// at the disk level instead.
fn parse_diskutil_list(xml: &[u8]) -> Result<Vec<Volume>, String> {
    let value = plist::Value::from_reader(Cursor::new(xml)).map_err(|e| e.to_string())?;
    let root = value
        .as_dictionary()
        .ok_or_else(|| "plist root is not a dictionary".to_string())?;
    let disks = root
        .get("AllDisksAndPartitions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "AllDisksAndPartitions missing".to_string())?;

    let mut volumes = Vec::new();
    for disk in disks {
        let Some(disk_dict) = disk.as_dictionary() else {
            continue;
        };
        match disk_dict.get("Partitions") {
            Some(partitions) => {
                if let Some(parts) = partitions.as_array() {
                    for part in parts {
                        if let Some(volume) = part.as_dictionary().and_then(volume_from_dict) {
                            volumes.push(volume);
                        }
                    }
                }
            }
            None => {
                if let Some(volume) = volume_from_dict(disk_dict) {
                    volumes.push(volume);
                }
            }
        }
    }
    Ok(volumes)
}

/// Read identity and mount state from one `diskutil list -plist` dict.
fn volume_from_dict(dict: &plist::Dictionary) -> Option<Volume> {
    let device = dict.get("DeviceIdentifier")?.as_string()?.to_string();
    Some(Volume {
        device,
        uuid: dict
            .get("VolumeUUID")
            .and_then(|v| v.as_string())
            .map(str::to_string),
        mount_point: dict
            .get("MountPoint")
            .and_then(|v| v.as_string())
            .map(str::to_string),
    })
}

/// `diskutil list -plist` can omit `VolumeUUID` (typically on an unmounted
/// partition); `diskutil info -plist` reads it from the filesystem itself.
fn info_volume_uuid(device: &str) -> Option<String> {
    let out = Command::new("diskutil")
        .args(["info", "-plist", device])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    plist_string(&out.stdout, "VolumeUUID")
}

/// Walk `system_profiler SPUSBDataType -json` collecting
/// `bsd_name -> (vendor_id, product_id)`. The ids sit on the USB device node
/// while `bsd_name` sits on it or on a descendant (typically `media[]`), so
/// ids are inherited down the walk.
fn collect_usb_ids(
    node: &Json,
    inherited: Option<(u16, u16)>,
    out: &mut HashMap<String, (u16, u16)>,
) {
    match node {
        Json::Object(map) => {
            let own = match (
                map.get("vendor_id")
                    .and_then(|v| v.as_str())
                    .and_then(parse_hex_u16),
                map.get("product_id")
                    .and_then(|v| v.as_str())
                    .and_then(parse_hex_u16),
            ) {
                (Some(vid), Some(pid)) => Some((vid, pid)),
                _ => None,
            };
            let ids = own.or(inherited);
            if let Some(bsd) = map.get("bsd_name").and_then(|v| v.as_str()) {
                if let Some(ids) = ids {
                    out.insert(bsd.to_string(), ids);
                }
            }
            for value in map.values() {
                collect_usb_ids(value, ids, out);
            }
        }
        Json::Array(items) => {
            for item in items {
                collect_usb_ids(item, inherited, out);
            }
        }
        _ => {}
    }
}

/// VID/PID of every USB disk `system_profiler` reports. Best effort: an empty
/// map leaves the ids unknown rather than failing enumeration.
fn usb_vid_pid_map() -> HashMap<String, (u16, u16)> {
    let mut map = HashMap::new();
    let out = match Command::new("system_profiler")
        .args(["SPUSBDataType", "-json"])
        .output()
    {
        Ok(out) if out.status.success() => out.stdout,
        Ok(out) => {
            debug!(
                "system_profiler failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return map;
        }
        Err(e) => {
            debug!("system_profiler unavailable: {}", e);
            return map;
        }
    };
    match serde_json::from_slice::<Json>(&out) {
        Ok(json) => collect_usb_ids(&json, None, &mut map),
        Err(e) => debug!("system_profiler JSON parse failed: {}", e),
    }
    map
}

/// Strip one partition suffix: `disk2s1` -> `disk2` (`disk2` -> `None`).
fn whole_disk_name(device: &str) -> Option<&str> {
    let split = device.rfind('s')?;
    let (head, tail) = device.split_at(split);
    let suffix = &tail[1..];
    if head.ends_with(|c: char| c.is_ascii_digit()) && !suffix.is_empty() && suffix.chars().all(|c: char| c.is_ascii_digit())
    {
        Some(head)
    } else {
        None
    }
}

/// Resolve the VID/PID of a partition: `media[].bsd_name` names the whole disk
/// (`disk2`), so `disk2s2` also tries itself as `disk2`.
fn ids_for_device(map: &HashMap<String, (u16, u16)>, device: &str) -> (Option<u16>, Option<u16>) {
    if let Some(&(vid, pid)) = map.get(device) {
        return (Some(vid), Some(pid));
    }
    if let Some(whole) = whole_disk_name(device) {
        if let Some(&(vid, pid)) = map.get(whole) {
            return (Some(vid), Some(pid));
        }
    }
    (None, None)
}

/// Enumerate the partitions of external physical disks (USB keys).
pub fn find_usb_partitions() -> Vec<UsbPartition> {
    let list = match Command::new("diskutil")
        .args(["list", "-plist", "external", "physical"])
        .output()
    {
        Ok(out) if out.status.success() => out.stdout,
        Ok(out) => {
            debug!(
                "diskutil list failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return Vec::new();
        }
        Err(e) => {
            debug!("diskutil unavailable: {}", e);
            return Vec::new();
        }
    };
    let mut volumes = match parse_diskutil_list(&list) {
        Ok(volumes) => volumes,
        Err(e) => {
            debug!("diskutil list parse failed: {}", e);
            return Vec::new();
        }
    };
    let ids = usb_vid_pid_map();

    // The whitelist lookup runs before mounting, so a UUID the list did not
    // carry has to come from the filesystem directly.
    for volume in &mut volumes {
        if volume.uuid.is_none() {
            volume.uuid = info_volume_uuid(&volume.device);
        }
    }

    volumes
        .into_iter()
        .map(|volume| {
            let (vendor_id, product_id) = ids_for_device(&ids, &volume.device);
            debug!(
                "diskutil: /dev/{} mount={:?} uuid={:?} id={:?}/{:?}",
                volume.device, volume.mount_point, volume.uuid, vendor_id, product_id
            );
            UsbPartition {
                devnode: format!("/dev/{}", volume.device),
                uuid: volume.uuid,
                vendor_id,
                product_id,
            }
        })
        .collect()
}

/// Mount a volume by device node. Already mounted counts as success
/// (`diskutil mount` would refuse it); exFAT/NTFS can take 5-10 s, the
/// callers already retry (plan_mac.md M2).
pub async fn mount_partition(devnode: &str) -> bool {
    if let Some(mount) = diskutil_mount_point(devnode).await {
        if !mount.is_empty() {
            debug!("{} is already mounted on {}", devnode, mount);
            return true;
        }
    }
    match TokioCommand::new("diskutil")
        .arg("mount")
        .arg(devnode)
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            debug!("Successfully mounted {}", devnode);
            true
        }
        Ok(out) => {
            debug!(
                "diskutil mount failed for {}: {}",
                devnode,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            debug!("Failed to execute diskutil mount: {}", e);
            false
        }
    }
}

/// Current mount point of a device, from `diskutil info -plist`.
async fn diskutil_mount_point(device: &str) -> Option<String> {
    let out = TokioCommand::new("diskutil")
        .args(["info", "-plist", device])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    plist_string(&out.stdout, "MountPoint")
}

/// Unmount by mount point: macOS exposes it directly, so there is no
/// `/proc`-style devnode resolution — `diskutil unmount` accepts the path.
pub async fn unmount_by_mount_point(mount_point: &Path) -> bool {
    match TokioCommand::new("diskutil")
        .arg("unmount")
        .arg(mount_point)
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            debug!("Successfully unmounted {}", mount_point.display());
            true
        }
        Ok(out) => {
            debug!(
                "diskutil unmount failed for {}: {}",
                mount_point.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            debug!("Failed to execute diskutil unmount: {}", e);
            false
        }
    }
}

/// macOS mounts external volumes under /Volumes.
pub fn is_removable_mount(mount_point: &str) -> bool {
    mount_point.starts_with("/Volumes/")
}

/// sysinfo reports BSD disk names (disk2s1 -> /dev/disk2s1).
pub fn to_devnode(name: &str) -> String {
    format!("/dev/{}", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>AllDisksAndPartitions</key>
    <array>
        <dict>
            <key>DeviceIdentifier</key><string>disk2</string>
            <key>Content</key><string>GUID_partition_scheme</string>
            <key>Partitions</key>
            <array>
                <dict>
                    <key>DeviceIdentifier</key><string>disk2s2</string>
                    <key>VolumeName</key><string>USBDATA</string>
                    <key>MountPoint</key><string>/Volumes/USBDATA</string>
                    <key>VolumeUUID</key><string>AAAA-BBBB</string>
                </dict>
                <dict>
                    <key>DeviceIdentifier</key><string>disk2s1</string>
                    <key>VolumeName</key><string>EFI</string>
                    <key>VolumeUUID</key><string>CCCC-DDDD</string>
                </dict>
            </array>
        </dict>
        <dict>
            <key>DeviceIdentifier</key><string>disk3</string>
            <key>VolumeName</key><string>STICK</string>
            <key>VolumeUUID</key><string>EEEE-FFFF</string>
            <key>MountPoint</key><string>/Volumes/STICK</string>
        </dict>
    </array>
</dict>
</plist>"#;

    #[test]
    fn parses_partitions_and_whole_disk_volumes() {
        let volumes = parse_diskutil_list(LIST_PLIST.as_bytes()).unwrap();
        assert_eq!(volumes.len(), 3);
        assert_eq!(volumes[0].device, "disk2s2");
        assert_eq!(volumes[0].uuid.as_deref(), Some("AAAA-BBBB"));
        assert_eq!(volumes[0].mount_point.as_deref(), Some("/Volumes/USBDATA"));
        // Unmounted partition: no MountPoint key.
        assert_eq!(volumes[1].device, "disk2s1");
        assert_eq!(volumes[1].mount_point, None);
        // Whole-disk volume (no partition table).
        assert_eq!(volumes[2].device, "disk3");
        assert_eq!(volumes[2].uuid.as_deref(), Some("EEEE-FFFF"));
    }

    #[test]
    fn system_profiler_ids_reach_media_children() {
        let json: Json = serde_json::from_str(
            r#"{"SPUSBDataType":[{"_items":[
                {"_name":"hub"},
                {"_name":"SanDisk Ultra","vendor_id":"0x0781","product_id":"0x5591",
                 "media":[{"_name":"SanDisk Ultra Media","bsd_name":"disk2"}]}
            ]}]}"#,
        )
        .unwrap();
        let mut map = HashMap::new();
        collect_usb_ids(&json, None, &mut map);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("disk2"), Some(&(1921, 21905)));
    }

    #[test]
    fn partitions_resolve_to_their_whole_disk_ids() {
        let mut map = HashMap::new();
        map.insert("disk2".to_string(), (1921u16, 21905u16));
        assert_eq!(ids_for_device(&map, "disk2s2"), (Some(1921), Some(21905)));
        assert_eq!(ids_for_device(&map, "disk2"), (Some(1921), Some(21905)));
        assert_eq!(ids_for_device(&map, "disk4s1"), (None, None));
    }

    #[test]
    fn whole_disk_name_strips_one_partition_suffix() {
        assert_eq!(whole_disk_name("disk2s1"), Some("disk2"));
        assert_eq!(whole_disk_name("disk10s2"), Some("disk10"));
        assert_eq!(whole_disk_name("disk2"), None);
        assert_eq!(whole_disk_name("disk images"), None);
    }
}

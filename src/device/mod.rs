//! Cross-platform device layer.
//!
//! One backend per supported OS, selected at compile time:
//! `linux.rs` (udev + udisksctl), `macos.rs` (diskutil), `windows.rs`
//! (WMI). Any other target fails to compile instead of silently
//! falling back to a non-functional backend.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(target_os = "windows")]
pub use windows::*;

/// A USB block partition with the metadata needed to correlate it to a
/// hotplug event (vendor_id/product_id) and to a backup config (uuid).
#[derive(Debug, Clone)]
pub struct UsbPartition {
    pub devnode: String,
    pub uuid: Option<String>,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
}

/// Return the USB partition matching both a filesystem UUID and, when
/// provided, a vendor/product id pair. Matching on VID/PID avoids
/// picking up another plugged-in key when several are present.
pub fn find_partition_by_uuid(
    uuid: &str,
    vendor_id: Option<u16>,
    product_id: Option<u16>,
) -> Option<UsbPartition> {
    find_usb_partitions().into_iter().find(|part| {
        let uuid_match = part.uuid.as_deref() == Some(uuid);
        let ids_match = match (vendor_id, product_id) {
            (Some(v), Some(p)) => part.vendor_id == Some(v) && part.product_id == Some(p),
            _ => true,
        };
        uuid_match && ids_match
    })
}

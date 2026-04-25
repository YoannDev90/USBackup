use nusb::hotplug::HotplugEvent;

mod handler;
mod models;
mod notifications;
mod storage;
mod tui;

use crate::handler::trigger_backup;
use crate::models::device::DeviceAction;
use crate::storage::load_config;
use crate::tui::{TuiEvent, run_tui};
use std::sync::mpsc;
use std::thread;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialise le logger (se configure via RUST_LOG)
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let (tx, rx) = mpsc::channel();
    let tx_hotplug = tx.clone();

    // Fil de surveillance hotplug (arrière-plan)
    thread::spawn(move || {
        let config = load_config();
        let watch = match nusb::watch_devices() {
            Ok(w) => w,
            Err(e) => {
                let _ = tx_hotplug.send(TuiEvent::Log(format!("Erreur Initialisation : {}", e)));
                return;
            }
        };

        for event in futures_lite::stream::block_on(watch) {
            match event {
                HotplugEvent::Connected(device) => {
                    let vid = device.vendor_id();
                    let pid = device.product_id();
                    let product = device.product_string().unwrap_or("Inconnu").to_string();
                    let device_key = format!("{:04x}:{:04x}", vid, pid);

                    let _ = tx_hotplug.send(TuiEvent::DeviceConnected(product.clone()));

                    if let Some(dev_conf) = config.devices.get(&device_key).cloned() {
                        if dev_conf.action == DeviceAction::Whitelist {
                            let tx_backup = tx_hotplug.clone();
                            thread::spawn(move || {
                                let _ =
                                    tx_backup.send(TuiEvent::BackupStarted(dev_conf.name.clone()));
                                trigger_backup(&dev_conf);
                                let _ =
                                    tx_backup.send(TuiEvent::BackupSuccess(dev_conf.name.clone()));
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    });

    // L'UI prend le contrôle du thread principal
    run_tui(rx)?;

    Ok(())
}

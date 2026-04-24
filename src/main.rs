use colored::Colorize;
use nusb::hotplug::HotplugEvent;

mod handler;
mod models;
mod notifications;
mod storage;

use crate::handler::{ask_user_action, handle_error, trigger_backup};
use crate::models::device::{BackupRule, DeviceAction, DeviceConfig};
use crate::storage::{load_config, save_config};

fn main() {
    println!("{}", "=== USBackup : Agent 24h/24 ===".bright_yellow());
    let mut config = load_config();

    let watch = match nusb::watch_devices() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{} Erreur d'initialisation : {}", " ERR ".on_red(), e);
            return;
        }
    };

    println!("{}", "Surveillance active...".cyan());

    for event in futures_lite::stream::block_on(watch) {
        let now = chrono::Local::now().format("%H:%M:%S");
        match event {
            HotplugEvent::Connected(device) => {
                let vid = device.vendor_id();
                let pid = device.product_id();
                let product = device.product_string().unwrap_or("Inconnu");
                let device_key = format!("{:04x}:{:04x}", vid, pid);

                println!("[{}] {} {} ({})", now, "(+)".green(), product, device_key);

                let action_to_take = config
                    .devices
                    .get(&device_key)
                    .map(|d| d.action.clone())
                    .unwrap_or(DeviceAction::AskEachTime);

                match action_to_take {
                    DeviceAction::Whitelist => {
                        if let Some(dev_conf) = config.devices.get(&device_key) {
                            trigger_backup(dev_conf);
                        }
                    }
                    DeviceAction::AskEachTime => {
                        let action = ask_user_action(vid, pid, product);
                        match action {
                            DeviceAction::Whitelist => {
                                let new_dev = DeviceConfig {
                                    name: product.to_string(),
                                    vendor_id: vid,
                                    product_id: pid,
                                    action: DeviceAction::Whitelist,
                                    backup_rules: vec![BackupRule {
                                        source_path: "/media/usb/data".to_string(),
                                        destination_path: format!("./backups/{}/", product),
                                        exclude: vec![],
                                    }],
                                };
                                config.devices.insert(device_key.clone(), new_dev);
                                save_config(&config);
                                println!("{}", "Périphérique ajouté à la whitelist.".green());
                                if let Some(dev_conf) = config.devices.get(&device_key) {
                                    trigger_backup(dev_conf);
                                }
                            }
                            DeviceAction::IgnoreForever => {
                                config.devices.insert(
                                    device_key,
                                    DeviceConfig {
                                        name: product.to_string(),
                                        vendor_id: vid,
                                        product_id: pid,
                                        action: DeviceAction::IgnoreForever,
                                        backup_rules: vec![],
                                    },
                                );
                                save_config(&config);
                                println!("{}", "Périphérique ignoré pour toujours.".yellow());
                            }
                            _ => {
                                println!("{}", "Action reportée.".cyan());
                            }
                        }
                    }
                    DeviceAction::IgnoreForever => {
                        println!(
                            "{} Périphérique configuré pour être ignoré.",
                            " SKIP ".on_black()
                        );
                    }
                }
            }
            HotplugEvent::Disconnected(device_id) => {
                println!("[{}] {} Déconnecté (ID: {:?})", now, "(-)".red(), device_id);
            }
        }
    }
}

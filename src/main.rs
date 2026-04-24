use colored::Colorize;
use nusb::hotplug::HotplugEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DeviceAction {
    Whitelist,
    IgnoreForever,
    AskEachTime,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BackupRule {
    pub source_path: String,
    pub destination_path: String,
    pub exclude: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeviceConfig {
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub action: DeviceAction,
    pub backup_rules: Vec<BackupRule>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppConfig {
    pub devices: HashMap<String, DeviceConfig>,
}

const CONFIG_PATH: &str = "backup_config.json";

fn load_config() -> AppConfig {
    if Path::new(CONFIG_PATH).exists() {
        let content = fs::read_to_string(CONFIG_PATH).expect("Erreur de lecture config");
        serde_json::from_str(&content).unwrap_or(AppConfig {
            devices: HashMap::new(),
        })
    } else {
        AppConfig {
            devices: HashMap::new(),
        }
    }
}

fn save_config(config: &AppConfig) {
    let content = serde_json::to_string_pretty(config).expect("Erreur de sérialisation");
    fs::write(CONFIG_PATH, content).expect("Erreur d'écriture config");
}

pub fn trigger_backup(device_config: &DeviceConfig) {
    println!(
        "{} Lancement des sauvegardes pour : {}",
        " -> ".blue(),
        device_config.name.green()
    );
    for rule in &device_config.backup_rules {
        println!(
            "   {} Synchronisation {} vers {}",
            " • ".cyan(),
            rule.source_path.yellow(),
            rule.destination_path.yellow()
        );
        // Ici : Logique de copie
    }
}

fn ask_user_action(vid: u16, pid: u16, product: &str) -> DeviceAction {
    println!(
        "\n{}",
        "=== Nouveau péripherique détecté ===".bright_magenta()
    );
    println!("Produit : {}", product.bright_white());
    println!("ID : {:04x}:{:04x}", vid, pid);
    println!("Que voulez-vous faire ?");
    println!("1. Whitelister (Ajouter à la config)");
    println!("2. Ignorer pour toujours");
    println!("3. Me redemander la prochaine fois");

    print!("Votre choix (1-3) : ");
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    match input.trim() {
        "1" => DeviceAction::Whitelist,
        "2" => DeviceAction::IgnoreForever,
        _ => DeviceAction::AskEachTime,
    }
}

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

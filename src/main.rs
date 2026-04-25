use nusb::hotplug::HotplugEvent;

mod handler;
mod models;
mod notifications;
mod storage;

use crate::handler::trigger_backup;
use crate::models::device::DeviceAction;
use crate::storage::load_config;
use log::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialise le logger
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    info!("Service USBackup démarré. En attente de périphériques...");

    let watch = match nusb::watch_devices() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Erreur Initialisation : {}", e);
            return Ok(());
        }
    };

    use futures_lite::StreamExt;
    let mut stream = watch;

    while let Some(event) = stream.next().await {
        match event {
            HotplugEvent::Connected(device) => {
                let vid = device.vendor_id();
                let pid = device.product_id();
                let product = device.product_string().unwrap_or("Inconnu").to_string();
                let device_key = format!("{:04x}:{:04x}", vid, pid);

                info!("[+] Appareil détecté : {} ({})", product, device_key);

                // Recharger la config à chaque connexion pour être à jour
                let mut config = load_config();

                if let Some(dev_conf) = config.devices.get(&device_key).cloned() {
                    if dev_conf.action == DeviceAction::Whitelist {
                        tokio::spawn(async move {
                            trigger_backup(&dev_conf).await;
                        });
                    }
                } else {
                    // Nouveau périphérique ou non configuré
                    if dialoguer::Confirm::new()
                        .with_prompt(format!(
                            "Nouveau périphérique ({}) détecté. Voulez-vous le configurer ?",
                            product
                        ))
                        .default(false)
                        .interact()?
                    {
                        crate::handler::run_wizard(vid, pid, &product, &mut config).await?;
                    }
                }
            }
            HotplugEvent::Disconnected(_) => {
                info!("[-] Appareil déconnecté");
            }
        }
    }

    Ok(())
}

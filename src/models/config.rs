use crate::models::device::DeviceConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppConfig {
    pub devices: HashMap<String, DeviceConfig>,
}

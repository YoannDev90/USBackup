use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AppConfig {
    pub secret_key: String,
    pub approved_uuids: Vec<String>,
}

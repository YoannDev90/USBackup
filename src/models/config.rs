use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AppConfig {
    pub approved_uuids: Vec<String>,
}

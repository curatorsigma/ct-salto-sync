use std::{fs::File, path::Path};

use serde::Deserialize;
use sqlx::{Pool, Sqlite};
use tracing::{event, Level};

#[derive(Debug, Deserialize)]
pub(crate) struct ConfigData {
    pub ct: ChurchToolsConfig,
    pub global: GlobalConfig,
    pub rooms: Vec<RoomConfig>,
}

#[derive(Debug)]
pub(crate) struct Config {
    pub ct: ChurchToolsConfig,
    pub db: Pool<Sqlite>,
    pub ct_client: reqwest::Client,
    pub global: GlobalConfig,
    pub rooms: Vec<RoomConfig>,
}
impl Config {
    async fn from_config_data(cd: ConfigData) -> Result<Config, Box<dyn std::error::Error>> {
        let connect_options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(crate::BOOKING_DATABASE_NAME)
            .create_if_missing(true);
        let db = sqlx::SqlitePool::connect_with(connect_options).await?;

        let ct_client = crate::ct::create_client(&cd.ct.login_token)?;

        Ok(Config {
            ct: cd.ct,
            db,
            ct_client,
            global: cd.global,
            rooms: cd.rooms,
        })
    }

    pub async fn create() -> Result<Config, Box<dyn std::error::Error>> {
        let path = Path::new("/etc/ct-ta-sync/config.yaml");
        let f = match File::open(path) {
            Ok(x) => x,
            Err(e) => {
                event!(
                    Level::ERROR,
                    "config file /etc/ct-ta-sync/config.yaml not readable: {e}"
                );
                return Err(Box::new(e));
            }
        };
        let config_data: ConfigData = match serde_yaml::from_reader(f) {
            Ok(x) => x,
            Err(e) => {
                event!(Level::ERROR, "config file had syntax errors: {e}");
                return Err(Box::new(e));
            }
        };
        Config::from_config_data(config_data).await
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct GlobalConfig {
    /// How often should we sync? In s.
    pub sync_frequency: u64,
    pub log_level: String,
}

#[derive(Deserialize)]
pub(crate) struct ChurchToolsConfig {
    pub host: String,
    pub login_token: String,
}
impl std::fmt::Debug for ChurchToolsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("ChurchToolsConfig")
            .field("host", &self.host)
            .field("login_token", &"[redacated]")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
pub struct RoomConfig {
    pub ct_id: i32,
    pub salto_ext_id: String,
}


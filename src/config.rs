use std::{fs::File, path::Path};

use serde::Deserialize;
use tracing::{Level, event};

#[derive(Debug, Deserialize)]
pub(crate) struct ConfigData {
    pub ct: ChurchToolsConfigData,
    pub salto: SaltoConfigData,
    pub db: DbData,
    pub global: GlobalConfig,
    pub rooms: Vec<RoomConfig>,
}

fn default_pgsql_port() -> u16 {
    5432
}
#[derive(Deserialize)]
pub(crate) struct DbData {
    host: String,
    #[serde(default = "default_pgsql_port")]
    port: u16,
    database: String,
    username: String,
    password: String,
}
impl core::fmt::Debug for DbData {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("DbData")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("user", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}

#[derive(Deserialize)]
pub(crate) struct SaltoConfigData {
    pub base_url: String,
    pub username: String,
    pub password: String,
    #[serde(default = "u16::default")]
    pub timetable_id: u16,
}
impl core::fmt::Debug for SaltoConfigData {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("SaltoConfigData")
            .field("base_url", &self.base_url)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}
#[derive(Debug)]
pub(crate) struct SaltoConfig {
    pub base_url: String,
    pub client: reqwest::Client,
    pub timetable_id: u16,
}

#[derive(Debug)]
pub(crate) struct Config {
    pub ct: ChurchToolsConfig,
    pub salto: SaltoConfig,
    pub db: sqlx::Pool<sqlx::Postgres>,
    pub global: GlobalConfig,
    pub rooms: Vec<RoomConfig>,
}
impl Config {
    async fn from_config_data(cd: ConfigData) -> Result<Config, Box<dyn core::error::Error>> {
        let ct_client = crate::ct::create_client(&cd.ct.login_token)?;
        let salto_client = crate::salto::create_client(&cd.salto).await?;

        // postgres settings
        let url = format!(
            "postgres://{}:{}@{}:{}/{}",
            cd.db.username, cd.db.password, cd.db.host, cd.db.port, cd.db.database
        );
        let pool = match sqlx::postgres::PgPool::connect(&url).await {
            Ok(x) => x,
            Err(e) => {
                event!(Level::ERROR, "Could not connect to postgres: {e}");
                return Err(Box::new(e));
            }
        };

        Ok(Config {
            salto: SaltoConfig {
                base_url: cd.salto.base_url,
                client: salto_client,
                timetable_id: cd.salto.timetable_id,
            },
            ct: ChurchToolsConfig {
                host: cd.ct.host,
                client: ct_client,
                group_magic_prefix: cd.ct.group_magic_prefix,
            },
            db: pool,
            global: cd.global,
            rooms: cd.rooms,
        })
    }

    pub async fn create() -> Result<Config, Box<dyn std::error::Error>> {
        let path = Path::new("/etc/salto-sync/config.yaml");
        let f = match File::open(path) {
            Ok(x) => x,
            Err(e) => {
                event!(
                    Level::ERROR,
                    "config file /etc/salto-sync/config.yaml not readable: {e}"
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

    /// Find the ExtId for this CT resource in the config
    pub fn room_ext_id(&self, resource_id: i64) -> Option<&String> {
        return self
            .rooms
            .iter()
            .find(|room| room.ct_id == resource_id)
            .map(|room| &room.salto_ext_id);
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct GlobalConfig {
    /// How often should we sync? In s.
    pub sync_frequency: u32,
    /// How long should a room be open to authorized persons before the actual booking begins? In
    /// m.
    #[serde(deserialize_with = "deserialize_timedelta_from_minutes")]
    pub prehold_time: chrono::TimeDelta,
    /// How long should a room be open to authorized persons after the actual booking has ended? In
    /// m.
    #[serde(deserialize_with = "deserialize_timedelta_from_minutes")]
    pub posthold_time: chrono::TimeDelta,
    /// At which level should the logger output information? (TRACE, DEBUG, INFO, WARN, ERROR)
    pub log_level: String,
}

fn deserialize_timedelta_from_minutes<'de, D>(
    deserializer: D,
) -> Result<chrono::TimeDelta, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let minutes: u32 = serde::de::Deserialize::deserialize(deserializer)?;
    Ok(chrono::TimeDelta::minutes(minutes.into()))
}

#[derive(Deserialize)]
pub(crate) struct ChurchToolsConfigData {
    pub host: String,
    pub login_token: String,
    pub group_magic_prefix: String,
}
impl core::fmt::Debug for ChurchToolsConfigData {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("ChurchToolsConfigData")
            .field("host", &self.host)
            .field("login_token", &"[redacated]")
            .field("group_magic_prefix", &self.group_magic_prefix)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct ChurchToolsConfig {
    pub host: String,
    pub client: reqwest::Client,
    pub group_magic_prefix: String,
}

#[derive(Debug, Deserialize)]
pub struct RoomConfig {
    pub ct_id: i64,
    pub salto_ext_id: String,
}

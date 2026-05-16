use serde::Deserialize;

use crate::{AppErr, error::Res};

#[derive(Debug, Deserialize)]
pub(crate) struct ConfigData {
    pub ct: ChurchToolsConfigData,
    pub salto: SaltoConfig,
    pub db: DbData,
    pub global: GlobalConfig,
    pub rooms: Vec<RoomConfig>,
}

fn default_pgsql_port() -> u16 {
    5432
}
#[derive(Deserialize, Clone)]
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

#[derive(Deserialize, Clone)]
pub(crate) struct SaltoConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
    #[serde(default = "u16::default")]
    pub timetable_id: u16,
}
impl core::fmt::Debug for SaltoConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("SaltoConfig")
            .field("base_url", &self.base_url)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("timetable_id", &self.timetable_id)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct AppConfig {
    pub ct: ChurchToolsConfig,
    pub salto: SaltoConfig,
    pub global: GlobalConfig,
    pub rooms: Vec<RoomConfig>,
}

pub struct ConnectionStates {
    pub salto_client: reqwest::Client,
    pub ct_client: reqwest::Client,
    pub db: sqlx::Pool<sqlx::Postgres>,
}

pub async fn establish_connections(cd: &ConfigData) -> Res<ConnectionStates> {
    let ct_client =
        crate::ct::create_client(&cd.ct.login_token).map_err(AppErr::ChurchToolsError)?;
    let salto_client = crate::salto::create_client(&cd.salto)
        .await
        .map_err(AppErr::SaltoError)?;

    // postgres settings
    let url = format!(
        "postgres://{}:{}@{}:{}/{}",
        cd.db.username, cd.db.password, cd.db.host, cd.db.port, cd.db.database
    );

    let pool = sqlx::postgres::PgPool::connect(&url)
        .await
        .map_err(AppErr::DbConnectionError)?;

    Ok(ConnectionStates {
        salto_client,
        ct_client,
        db: pool,
    })
}

impl From<ConfigData> for AppConfig {
    fn from(value: ConfigData) -> Self {
        AppConfig {
            salto: value.salto,
            ct: ChurchToolsConfig {
                host: value.ct.host.clone(),
                group_magic_prefix: value.ct.group_magic_prefix.clone(),
            },
            global: value.global,
            rooms: value.rooms,
        }
    }
}

impl AppConfig {
    /// Find the `ExtId` for this CT resource in the config
    pub fn room_ext_id(&self, resource_id: i64) -> Option<&String> {
        self.rooms
            .iter()
            .find(|room| room.ct_id == resource_id)
            .map(|room| &room.salto_ext_id)
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
    pub group_magic_prefix: String,
}

#[derive(Debug, Deserialize)]
pub struct RoomConfig {
    pub ct_id: i64,
    pub salto_ext_id: String,
}

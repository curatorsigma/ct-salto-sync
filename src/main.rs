//! Pulls bookings from CT, pushes the users allowed in those bookings to Salto.
#![allow(non_snake_case)]

use core::str::FromStr;
use std::env;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;

use ct::CTApiError;
use db::DBError;
use salto::SaltoApiError;
use tokio::sync::Mutex;
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, prelude::*};
use tracing_subscriber::{filter, fmt::format::FmtSpan};

use crate::config::{AppConfig, ConfigData, establish_connections};

pub use self::error::{AppErr, Res};

mod config;
mod ct;
mod db;
mod error;
mod pull_bookings;
mod salto;

/// A single booking for a room
#[derive(Debug, PartialEq)]
struct Booking {
    /// The ID of this booking. This is used to update bookings when they are updated in CT.
    id: i64,
    /// the ID of the resource for this booking.
    /// NOTE: this is NOT the ID of the booking, but of the resource in CT.
    /// This ID is used for matching ressources against rooms defined in the config.
    resource_id: i64,
    /// The booking starts at...
    /// ALL DATETIMES ARE UTC.
    start_time: chrono::DateTime<Utc>,
    /// The booking ends at...
    end_time: chrono::DateTime<Utc>,
    /// Transponder IDs of other users that are permitted for this booking.
    ///
    /// Other users are permitted iff they are members of a CT-group with id gid such that
    /// `<magic_prefix><gid>` is contained in the description, separated from
    /// other content by whitespace
    permitted_transponders: Vec<i64>,
}

enum InShutdown {
    Yes,
    No,
}

/// Something went wrong while gathering Information from CT into the DB
#[derive(Debug)]
pub enum GatherError {
    DB(crate::db::DBError),
    CT(CTApiError),
    Salto(SaltoApiError),
}
impl core::fmt::Display for GatherError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::DB(x) => write!(f, "DBError: {x}"),
            Self::CT(x) => write!(f, "CTApiError: {x}"),
            Self::Salto(x) => write!(f, "SaltoApiError: {x}"),
        }
    }
}
impl core::error::Error for GatherError {}
impl From<DBError> for GatherError {
    fn from(value: DBError) -> Self {
        Self::DB(value)
    }
}
impl From<CTApiError> for GatherError {
    fn from(value: CTApiError) -> Self {
        Self::CT(value)
    }
}
impl From<SaltoApiError> for GatherError {
    fn from(value: SaltoApiError) -> Self {
        Self::Salto(value)
    }
}

async fn signal_handler(
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
    shutdown_tx: tokio::sync::watch::Sender<InShutdown>,
) -> Result<(), std::io::Error> {
    let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        Ok(x) => x,
        Err(e) => {
            error!("Failed to install SIGTERM listener: {e} Aborting.");
            shutdown_tx.send_replace(InShutdown::Yes);
            return Err(e);
        }
    };
    let mut sighup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(x) => x,
        Err(e) => {
            error!("Failed to install SIGHUP listener: {e} Aborting.");
            shutdown_tx.send_replace(InShutdown::Yes);
            return Err(e);
        }
    };
    let mut sigint = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    {
        Ok(x) => x,
        Err(e) => {
            error!("Failed to install SIGINT listener: {e} Aborting.");
            shutdown_tx.send_replace(InShutdown::Yes);
            return Err(e);
        }
    };
    // wait for a shutdown signal
    tokio::select! {
        // shutdown the signal handler when some other process signals a shutdown
        _ = watcher.changed() => {}
        _ = sigterm.recv() => {
            info!("Got SIGTERM. Shuting down.");
            shutdown_tx.send_replace(InShutdown::Yes);
        }
        _ = sighup.recv() => {
            info!("Got SIGHUP. Shuting down.");
            shutdown_tx.send_replace(InShutdown::Yes);
        }
        _ = sigint.recv() => {
            info!("Got SIGINT. Shuting down.");
            shutdown_tx.send_replace(InShutdown::Yes);
        }
        x = tokio::signal::ctrl_c() =>  {
            match x {
                Ok(()) => {
                    info!("Received Ctrl-c. Shutting down.");
                    shutdown_tx.send_replace(InShutdown::Yes);
                }
                Err(err) => {
                    error!("Unable to listen for shutdown signal: {}", err);
                    shutdown_tx.send_replace(InShutdown::Yes);
                }
            }
        }
    };

    Ok(())
}

fn init_logging(level: &String) {
    let my_crate_filter = EnvFilter::new("salto_sync");
    let level_filter = filter::LevelFilter::from_str(level).unwrap_or_else(|_| {
        // need to println because trace not yet configured
        println!("Unknown log level '{level}', defaulting to INFO");
        filter::LevelFilter::INFO
    });

    let subscriber = tracing_subscriber::registry().with(my_crate_filter).with(
        tracing_subscriber::fmt::layer()
            .compact()
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_line_number(true)
            .with_filter(level_filter),
    );
    tracing::subscriber::set_global_default(subscriber).expect("static tracing config");

    let version = env!("CARGO_PKG_VERSION");
    let name = env!("CARGO_PKG_NAME");

    tracing::info!(
        "{name} v{version} started: Config loaded, ct & salto login successful. Starting sync..."
    );
}

const DEFAULT_CONFIG_PATH: &str = "/etc/salto-sync/config.yaml";

/*
 * Allowing config path override for testing purposes
 * can be done via '-c <path>'
 */
fn get_config_path() -> String {
    let args: Vec<String> = env::args().collect();

    args.windows(2)
        .find(|window| window[0].starts_with("-c"))
        .map_or(DEFAULT_CONFIG_PATH.to_string(), |window| window[1].clone())
}

fn read_config_data(path: String) -> Res<ConfigData> {
    let p = Path::new(&path);
    let f = File::open(p).map_err(|e| AppErr::IO(path, e))?;
    let configData: ConfigData = serde_yaml::from_reader(f).map_err(AppErr::ConfigParsingError)?;

    Ok(configData)
}

async fn init_db(db: &sqlx::Pool<sqlx::Postgres>) -> Result<(), Box<dyn core::error::Error>> {
    match sqlx::migrate!().run(db).await {
        Ok(()) => {
            tracing::debug!("Migrated DB successfully.");
            Ok(())
        }
        Err(e) => {
            tracing::error!("Error while migrating database: {e}. Aborting.");
            Err(e)?
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn core::error::Error>> {
    let configPath = get_config_path();
    let configData = read_config_data(configPath)?;
    let state = establish_connections(&configData).await?;
    let connections = Arc::new(Mutex::new(state));
    let config: Arc<AppConfig> = Arc::new(configData.into());
    init_logging(&config.global.log_level);
    init_db(&connections.lock().await.db).await?;

    // cancellation channel
    let (tx, rx) = tokio::sync::watch::channel(InShutdown::No);

    let bookings_handle = tokio::spawn(pull_bookings::synchronization_loop(
        config.clone(),
        connections.clone(),
        rx,
    ));

    // start the Signal handler
    let signal_handle = tokio::spawn(signal_handler(tx.subscribe(), tx.clone()));

    // Join both tasks
    let (bookings_res, signal_res) = tokio::join!(bookings_handle, signal_handle);
    bookings_res?;
    signal_res??;

    Ok(())
}

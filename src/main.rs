use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;

use ct::CTApiError;
use db::DBError;
use tracing::{error, info};
use tracing_subscriber::{filter, fmt::format::FmtSpan};
use tracing_subscriber::{prelude::*, EnvFilter};

mod config;
mod ct;
mod db;
mod pull_bookings;
mod write_staging;

const BOOKING_DATABASE_NAME: &str = ".bookings.db";

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
    /// Ct-id of the person that created this booking
    created_by: i64,
    /// The description for this booking as set in CT
    description: String,
}

#[derive(Debug, PartialEq)]
struct Person {
    /// ID for this person in our db
    id: i64,
    /// ID for this person in CT
    ct_id: i32,
    /// ID of this persons transponder as given in CT
    transponder_id: i32,
    /// ID of this person in Salto
    salt_ext_id: String,
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
}
impl core::fmt::Display for GatherError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::DB(x) => write!(f, "DBError: {x}"),
            Self::CT(x) => write!(f, "CTApiError: {x}"),
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


async fn signal_handler(
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
    shutdown_tx: tokio::sync::watch::Sender<InShutdown>,
) -> Result<(), std::io::Error> {
    let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
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
    let mut sigint = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
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
                    // we also shut down in case of error
                    shutdown_tx.send_replace(InShutdown::Yes);
                }
            }
        }
    };

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Arc::new(config::Config::create().await?);
    // Setup tracing

    let my_crate_filter = EnvFilter::new("ct_ta_sync");
    let level_filter = filter::LevelFilter::from_str(&config.global.log_level)?;
    let subscriber = tracing_subscriber::registry().with(my_crate_filter).with(
        tracing_subscriber::fmt::layer()
            .compact()
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_line_number(true)
            .with_filter(level_filter),
    );
    tracing::subscriber::set_global_default(subscriber).expect("static tracing config");

    // migrate the database
    sqlx::migrate!().run(&config.db).await?;

    // the external temperature
    // cancellation channel
    let (tx, rx) = tokio::sync::watch::channel(InShutdown::No);

    let bookings_handle = tokio::spawn(pull_bookings::keep_bookings_up_to_date(config.clone(), rx, tx.clone()));

    let write_handle = tokio::spawn(write_staging::keep_staging_table_up_to_date(config.clone(), tx.subscribe(), tx.clone()));

    // start the Signal handler
    let signal_handle = tokio::spawn(signal_handler(tx.subscribe(), tx.clone()));

    // Join both tasks
    let (bookings_res, write_res, signal_res) = tokio::join!(
        bookings_handle,
        write_handle,
        signal_handle
    );
    bookings_res?;
    write_res?;
    signal_res??;

    Ok(())
}

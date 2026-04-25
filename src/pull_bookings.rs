//! Get data from Churchtools

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::{
    Booking, GatherError, InShutdown,
    config::{AppConfig, ConnectionStates},
    ct::api_get_relevant_bookings,
    db::db_overwrite_staging_table_with,
    salto::{SaltoApiError, api_get_ext_ids},
};

/// The data we want salto to write into their system in their format.
pub struct StagingEntry {
    pub ext_user_id: String,
    // format is
    // {{"2014F70541B7A6C0C90008DD1AB1BAB0",0,2025-11-24T13:00:00,2025-11-24T17:20:59}, ...}
    // {{"zone-ext-id",0,start,end}} where start and end are given in "RFC3339", but are
    // interpreted as local time and not as UTC
    pub ext_zone_id_list: String,
}

// other random shit to add so salto works:
// - Action INTEGER NOT NULL DEFAULT 2 (UPDATE only)
// - drop content when no longer wanted

fn salto_single_permitted_zone_format(
    zone_ext_id: &str,
    timetable_id: u16,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
) -> String {
    let time_format = chrono::format::StrftimeItems::new("%Y-%m-%dT%H:%M:%S");
    //STFO: It's kind of wired that we're using local time here
    // -> This is quite obfuscating since we're running the app in docker,
    // so we would take the local time of the container, which is currently also UTC
    // => Why would we need local time in this case anyway?
    format!(
        "{{\"{zone_ext_id}\",{},{},{}}}",
        timetable_id,
        start_time
            .with_timezone(&chrono::Local)
            .format_with_items(time_format.clone()),
        end_time
            .with_timezone(&chrono::Local)
            .format_with_items(time_format.clone()),
    )
}

fn match_zones_for_active_bookings(
    config: &Arc<AppConfig>,
    bookings: Vec<Booking>,
) -> HashMap<i64, String> {
    let mut ext_zone_id_list_by_transponder = HashMap::<i64, String>::new();
    let now = chrono::Utc::now();
    for booking in bookings {
        // the posthold time has already ended or the prehold time will start in more then
        // sync_frequency seconds - ignore this booking
        if now > booking.end_time + config.global.posthold_time
            || now
                < booking.start_time
                    - config.global.prehold_time
                    - chrono::TimeDelta::seconds(config.global.sync_frequency.into())
        {
            continue;
        }
        let Some(zone_ext_id) = config.room_ext_id(booking.resource_id) else {
            warn!(
                "Got booking for room {}, but could not find its salto ExtId.",
                booking.resource_id
            );
            continue;
        };
        let additional_zone = salto_single_permitted_zone_format(
            zone_ext_id,
            config.salto.timetable_id,
            booking.start_time,
            booking.end_time,
        );

        // matching zones for all active bookings to the transponder id
        for transponder in booking.permitted_transponders {
            ext_zone_id_list_by_transponder
                .entry(transponder)
                .and_modify(|l| {
                    l.push(',');
                    l.push_str(&additional_zone);
                })
                .or_insert(additional_zone.to_string());
        }
    }

    ext_zone_id_list_by_transponder
}

fn to_staging_entries(
    personExtIds: HashMap<i64, Option<String>>,
    activeTransponderZones: HashMap<i64, String>,
) -> Vec<StagingEntry> {
    personExtIds
        .into_iter()
        .filter_map(|(transponder, extIdOpt)| {
            extIdOpt.and_then(|extId| {
                Some(StagingEntry {
                    ext_user_id: extId,
                    ext_zone_id_list: activeTransponderZones.get(&transponder)?.to_string(),
                })
            })
        })
        .collect::<Vec<_>>()
}

/// A single run of the sync - get bookings from CT and write them to the staging table.
async fn run_sync_once(
    config: Arc<AppConfig>,
    connections: Arc<Mutex<ConnectionStates>>,
) -> Result<(), GatherError> {
    info!("Starting synchronization ...");

    let bookings = {
        let guard = connections.lock().await;
        api_get_relevant_bookings(&config, &*guard).await?
    };
    let activeTransponderZones = match_zones_for_active_bookings(&config, bookings);

    let mut personExtIdsResult = api_get_ext_ids(
        config.clone(),
        connections.clone(),
        activeTransponderZones.keys(),
    )
    .await;

    personExtIdsResult = match personExtIdsResult {
        Err(SaltoApiError::CredentialsInvalid(msg) | SaltoApiError::CredentialsExpired(msg)) => {
            debug!("Credentials error '{msg}', reauth and retry.");

            let conf = &config.clone().salto.salto_config_data;
            connections.lock().await.salto_client = crate::salto::create_client(conf).await?;
            api_get_ext_ids(config, connections.clone(), activeTransponderZones.keys()).await
        }
        _ => personExtIdsResult,
    };

    let stagingEntries = to_staging_entries(personExtIdsResult?, activeTransponderZones);

    info!("Got a total of {} staging entries", stagingEntries.len());
    db_overwrite_staging_table_with(&connections.lock().await.db, stagingEntries).await?;

    info!("Finished synchronization ...");
    Ok(())
}

/// Continuously pull Data from CT into the DB
pub async fn synchronization_loop(
    config: Arc<AppConfig>,
    connections: Arc<Mutex<ConnectionStates>>,
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
) {
    info!("Starting Synchronization thread (API pull, DB push)");
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
        config.global.sync_frequency.into(),
    ));
    interval.tick().await;

    loop {
        let syncResult = run_sync_once(config.clone(), connections.clone()).await;
        match syncResult {
            Ok(()) => {}
            Err(e) => {
                warn!("Error during synchronization process: {e}");
            }
        }

        // stop on cancellation or continue after the next tick
        tokio::select! {
            _ = watcher.changed() => {
                debug!("Shutting down data gatherer now.");
                return;
            }
            _ = interval.tick() => {}
        }
    }
}

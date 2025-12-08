//! Get data from Churchtools

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use tracing::{debug, info, trace, warn};

use crate::{
    Booking, GatherError, InShutdown,
    config::Config,
    ct::get_relevant_bookings,
    db::overwrite_staging_table_with,
    salto::{SaltoApiError, get_ext_ids_by_transponder},
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

/// Convert the Vec of bookings into a Vec of entries, one for each user, containing the zones that
/// user should get access to across all the bookings.
///
/// Translates transponder ids into `ExtIds`, "transposes" the structure, and formats the zones and
/// times into saltos format.
async fn convert_to_staging_entries(
    config: Arc<Config>,
    bookings: Vec<Booking>,
) -> Result<Vec<StagingEntry>, SaltoApiError> {
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

    trace!("now getting ext ids");
    let person_ext_ids_by_transponder =
        get_ext_ids_by_transponder(config, ext_zone_id_list_by_transponder.keys()).await?;
    trace!("got ext ids");
    Ok(person_ext_ids_by_transponder
        .into_iter()
        .filter_map(|(transponder, ext_id_opt)| {
            ext_id_opt.and_then(|ext_id| {
                Some(StagingEntry {
                    ext_user_id: ext_id,
                    ext_zone_id_list: ext_zone_id_list_by_transponder
                        .get(&transponder)?
                        .to_string(),
                })
            })
        })
        .collect::<Vec<_>>())
}

/// A single run of the sync - get bookings from CT and write them to the staging table.
async fn sync_once(config: Arc<Config>) -> Result<(), GatherError> {
    let bookings = get_relevant_bookings(&config).await?;
    let staging_entries = convert_to_staging_entries(config.clone(), bookings).await?;
    info!("got staging entries");
    info!("total of {} entries", staging_entries.len());
    overwrite_staging_table_with(&config.db, staging_entries).await?;
    info!("Overwrote staging table with new data.");
    Ok(())
}

/// Continuously pull Data from CT into the DB
pub async fn keep_bookings_up_to_date(
    config: Arc<Config>,
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
) {
    info!("Starting CT -> DB Sync task");
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
        config.global.sync_frequency.into(),
    ));
    interval.tick().await;

    loop {
        debug!("Now syncing from CT.");
        match sync_once(config.clone()).await {
            Ok(()) => {}
            Err(e) => {
                warn!("Failed to sync CT -> Staging Table: {e}");
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

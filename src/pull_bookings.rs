//! Get data from Churchtools

use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, info, trace, warn};

use crate::{config::Config, ct::get_relevant_bookings, GatherError, InShutdown};

async fn get_bookings_into_db(config: Arc<Config>) -> Result<(), GatherError> {
    let start = Utc::now().naive_utc().into();
    let end = start + chrono::TimeDelta::days(1);
    // get bookings from CT
    let bookings_from_ct = get_relevant_bookings(&config, &config.ct_client, start, end).await?;
    // get bookings from db
    let bookings_from_db = crate::db::get_bookings_in_timeframe(
        &config.db,
        start.and_time(chrono::NaiveTime::from_hms_opt(0, 0, 0).expect("statically good time")),
        end.and_time(chrono::NaiveTime::from_hms_opt(23, 59, 59).expect("statically good time")),
    )
    .await?;

    // compare the two sources
    // add new bookings
    trace!("in db: {bookings_from_db:?}");
    trace!("in ct: {bookings_from_ct:?}");
    let new_bookings = bookings_from_ct.iter().filter(|b| {
        !bookings_from_db
            .iter()
            .any(|x| x.id == b.id)
    });
    trace!(
        "Adding these bookings: {:?}",
        new_bookings.clone().collect::<Vec<_>>()
    );
    crate::db::insert_bookings(&config.db, new_bookings).await?;

    // remove bookings no longer present in ct
    let deprecated_bookings = bookings_from_db
        .iter()
        .map(|b| b.id)
        .filter(|&id| !bookings_from_ct.iter().any(|x| x.id == id));
    crate::db::delete_bookings(&config.db, deprecated_bookings).await?;

    // Update bookings that have changed times in CT
    let changed_bookings = bookings_from_ct.iter().filter(|b| {
        bookings_from_db
            .iter()
            .any(|x| x.id == b.id && x != *b)
    });
    crate::db::update_bookings(&config.db, changed_bookings).await?;
    Ok(())
}

/// Continuously pull Data from CT into the DB
pub async fn keep_bookings_up_to_date(
    config: Arc<Config>,
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
    shutdown_tx: tokio::sync::watch::Sender<InShutdown>,
) {
    info!("Starting CT -> DB Sync task");
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
        config.global.sync_frequency,
    ));
    interval.tick().await;

    loop {
        debug!("Gatherer starting new run.");
        // get new data
        let ct_to_db_res = get_bookings_into_db(config.clone()).await;
        match ct_to_db_res {
            Ok(()) => debug!("Successfully updated db."),
            Err(e) => {
                warn!("Failed to update db from CT. Error encountered: {e}");
            }
        }
        // prune old entries in db
        let db_prune_res = crate::db::prune_old_bookings(&config.db).await;
        match db_prune_res {
            Ok(x) => match x {
                0 => debug!("Successfully pruned db. Removed {x} old bookings."),
                y => info!("Successfully pruned db. Removed {y} old bookings."),
            },
            Err(e) => {
                warn!("Failed to prune db. Error encountered: {e}");
            }
        }
        // update the access mapping from bookings to users

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


//! Write to the staging table

use std::sync::Arc;

use crate::{InShutdown, config::Config};

pub async fn keep_staging_table_up_to_date(
    config: Arc<Config>,
    mut watcher: tokio::sync::watch::Receiver<InShutdown>,
    shutdown_tx: tokio::sync::watch::Sender<InShutdown>,
) {
    todo!()
    // loop
    // go through all bookings in the db
    // find persons that should have access from CT
    // get their salto ext id
    // push the relevant zones into their row in the staging table
}

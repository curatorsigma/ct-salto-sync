//! All the db-related functions

use sqlx::{PgPool, Postgres, Transaction};

use crate::pull_bookings::StagingEntry;

#[derive(Debug)]
pub enum DBError {
    StartTransaction(sqlx::Error),
    CommitTransaction(sqlx::Error),
    UpsertStaging(sqlx::Error),
    GetEntries(sqlx::Error),
    RemoveEntry(sqlx::Error),
}
impl core::fmt::Display for DBError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::StartTransaction(e) => {
                write!(f, "Cannot start transaction: {e}")
            }
            Self::CommitTransaction(e) => {
                write!(f, "Cannot commit transaction: {e}")
            }
            Self::UpsertStaging(e) => {
                write!(f, "Cannot upsert staging entry: {e}")
            }
            Self::GetEntries(e) => {
                write!(f, "Cannot get staging entries: {e}")
            }
            Self::RemoveEntry(e) => {
                write!(f, "Cannot remove staging entry: {e}")
            }
        }
    }
}
impl core::error::Error for DBError {}

async fn upsert_staging_entry(
    tx: &mut Transaction<'_, Postgres>,
    entry: &StagingEntry,
) -> Result<(), DBError> {
    sqlx::query!(
        "INSERT INTO salto_staging (ExtID, ExtZoneIDList)
            VALUES ($1, $2)
            ON CONFLICT (ExtID) DO
                UPDATE SET
                    ExtZoneIDList = $2,
                    ToBeProcessedBySalto = 1,
                    ProcessedDateTime = NULL,
                    ErrorCode = NULL,
                    ErrorMessage = NULL;",
        entry.ext_user_id,
        entry.ext_zone_id_list
    )
    .execute(&mut **tx)
    .await
    .map_err(DBError::UpsertStaging)?;
    Ok(())
}

async fn get_existing_entries_by_extid(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<impl Iterator<Item = String> + 'static, DBError> {
    Ok(sqlx::query!("SELECT ExtID FROM salto_staging;")
        .fetch_all(&mut **tx)
        .await
        .map_err(DBError::GetEntries)?
        .into_iter()
        .map(|record| record.extid))
}

async fn remove_entry_by_extid(
    tx: &mut Transaction<'_, Postgres>,
    ext_id: &str,
) -> Result<(), DBError> {
    sqlx::query!(
        "UPDATE salto_staging SET ExtZoneIDList = '' WHERE ExtID = $1;",
        ext_id
    )
    .execute(&mut **tx)
    .await
    .map(|_x| ())
    .map_err(DBError::RemoveEntry)
}

/// Ensures that the staging table contains exactly these entries
pub async fn overwrite_staging_table_with(
    pool: &PgPool,
    entries: Vec<StagingEntry>,
) -> Result<(), DBError> {
    let mut tx = pool.begin().await.map_err(DBError::StartTransaction)?;

    let existing_outdated_entries =
        get_existing_entries_by_extid(&mut tx)
            .await?
            .filter(|existing_ext_id| {
                entries
                    .iter()
                    .all(|new_entry| new_entry.ext_user_id != *existing_ext_id)
            });
    for entry in existing_outdated_entries {
        remove_entry_by_extid(&mut tx, &entry).await?;
    }

    for entry in entries {
        upsert_staging_entry(&mut tx, &entry).await?;
    }

    tx.commit().await.map_err(DBError::CommitTransaction)?;
    Ok(())
}

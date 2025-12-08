//! Everything directly interfacing with CT.

use std::collections::HashMap;

use itertools::Itertools;
use reqwest::header;
use serde::Deserialize;
use tracing::warn;

use crate::{Booking, config::Config};

/// Create a Client with cookie store that sends the correct auth header each time
///
/// CT will honor the session cookie, and relogin when the cookie is stable because the correct
/// auth header is also sent.
pub fn create_client(login_token: &str) -> Result<reqwest::Client, reqwest::Error> {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );
    let mut auth_value = header::HeaderValue::from_str(&format!("Login {login_token}"))
        .expect("statically good header");
    auth_value.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, auth_value);
    reqwest::Client::builder()
        .cookie_store(true)
        .default_headers(headers)
        .use_rustls_tls()
        .build()
}

/// Something went wrong with CT
#[derive(Debug)]
pub enum CTApiError {
    GetBookings(reqwest::Error),
    GetGroupMembers(reqwest::Error),
    GetAppointments(reqwest::Error),
    Deserialize,
    Utf8Decode,
    ParseTime(chrono::ParseError, String),
    NoCalculatedDateTimeOnDay(i64, String),
    NoCalculatedDateTime(i64),
}
impl core::fmt::Display for CTApiError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::GetBookings(e) => {
                write!(f, "Cannot get bookings. reqwest Error: {e}")
            }
            Self::GetGroupMembers(e) => {
                write!(f, "Cannot get group members. reqwest Error: {e}")
            }
            Self::GetAppointments(e) => {
                write!(f, "Cannot get appointments. reqwest Error: {e}")
            }
            Self::Deserialize => {
                write!(f, "Cannot deserialize the response.")
            }
            Self::Utf8Decode => {
                write!(f, "Cannot decode the message bytes as utf-8.")
            }
            Self::ParseTime(e, s) => {
                write!(
                    f,
                    "Cannot parse a time contained in CTs response. chrono Error: {e}. response from CT: {s}."
                )
            }
            Self::NoCalculatedDateTimeOnDay(appointment, day) => {
                write!(
                    f,
                    "Appointment {appointment} has no calculated datetime on {day}."
                )
            }
            Self::NoCalculatedDateTime(appointment) => {
                write!(f, "Appointment {appointment} has no calculated datetime.")
            }
        }
    }
}
impl core::error::Error for CTApiError {}

#[derive(Debug, Deserialize)]
struct CTBookingsResponse {
    data: Vec<BookingsData>,
}
#[derive(Debug, Deserialize)]
struct BookingsData {
    base: BookingsDataBase,
    calculated: BookingsDataCalculated,
}

#[derive(Debug, Deserialize)]
struct BookingsDataBase {
    /// this is the bookings ID
    id: i64,
    #[serde(rename = "resourceId")]
    resource_id: i64,
    appointment: Option<AppointmentData>,
    /// Note for this Booking in CT - required because it contains the group names to add access to
    description: Option<String>,
    meta: BookingMeta,
}

#[derive(Debug, Deserialize)]
struct BookingMeta {
    #[serde(rename = "createdPerson")]
    created_person: PersonData,
}

#[derive(Debug, Deserialize)]
struct PersonData {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct AppointmentData {
    /// this is the resources ID
    id: i64,
    #[serde(rename = "calendarId")]
    calendar_id: i64,
}

#[derive(Debug, Deserialize)]
struct BookingsDataCalculated {
    #[serde(rename = "startDate")]
    start_date: String,
    #[serde(rename = "endDate")]
    end_date: String,
}

/// The full struct returned from CTs /api/calendar/{id}/appointments.
#[derive(Debug, Deserialize)]
struct CTAppointmentResponse {
    data: FullAppointmentData,
}

/// A useless intermediate level struct
#[derive(Debug, Deserialize)]
struct FullAppointmentData {
    /// A repeating appointment. Takes precedence when both `calculated_dates` and `calculated` are
    /// given.
    #[serde(rename = "calculatedDates")]
    calculated_dates: Option<HashMap<String, Timeframe>>,
    /// A single, nonrepeating appointment
    calculated: Option<Timeframe>,
}

#[derive(Debug, Deserialize)]
pub struct Timeframe {
    #[serde(rename = "startDate")]
    start_date: String,
    #[serde(rename = "endDate")]
    end_date: String,
}

/// Get an appointment (Calendar-Entry) from CT by its ID
///
/// Resource bookings that are linked to a calendar entry show the time of the calendar entry, not
/// of the resource.
///
/// # INPUTS
///     `config`
///     `appointment_id`: ID of the appointment (calender entry)
///     `calendar_id`: ID of the calendar
///     `day`: YYYY-mm-dd representation of the day on which to take the date for a repeating
///     appointment
pub async fn get_appointment(
    config: &Config,
    appointment_id: i64,
    calendar_id: i64,
    day: &str,
) -> Result<Timeframe, CTApiError> {
    let response = match config
        .ct
        .client
        .get(format!(
            "https://{}/api/calendars/{}/appointments/{}",
            config.ct.host, calendar_id, appointment_id
        ))
        .send()
        .await
    {
        Ok(x) => match x.text().await {
            Ok(text) => {
                let deser_res: Result<CTAppointmentResponse, _> = serde_json::from_str(&text);
                if let Ok(y) = deser_res {
                    y
                } else {
                    warn!("There was an error parsing the return value from CT.");
                    warn!("The complete text received was: {text}");
                    return Err(CTApiError::Deserialize);
                }
            }
            Err(e) => {
                warn!("There was an error reading the response from CT as utf-8: {e}");
                return Err(CTApiError::Utf8Decode);
            }
        },
        Err(e) => {
            warn!("There was a problem getting a response from CT");
            return Err(CTApiError::GetAppointments(e));
        }
    };
    if let Some(mut calculated_dates) = response.data.calculated_dates {
        calculated_dates
            .remove(day)
            .ok_or_else(|| CTApiError::NoCalculatedDateTimeOnDay(appointment_id, day.to_string()))
    } else {
        response
            .data
            .calculated
            .ok_or(CTApiError::NoCalculatedDateTime(appointment_id))
    }
}

/// Find all `<magic_prefix><group-id>` separated by whitespace in the description and parse out
/// the group-ids into a vec
fn groups_from_description(description: &str, magic_prefix: &str) -> Vec<i64> {
    description
        .split_whitespace()
        .filter_map(|word| word.strip_prefix(magic_prefix))
        .filter_map(|group_id| group_id.parse().ok())
        .collect()
}

#[derive(Debug, Deserialize)]
struct CtGroupMemberResponse {
    data: Vec<GroupMemberData>,
}

#[derive(Debug, Deserialize)]
struct GroupMemberData {
    #[serde(rename = "personFields")]
    person_fields: PersonFields,
}

#[derive(Debug, Deserialize)]
struct PersonFields {
    #[serde(rename = "transponderId")]
    transponder_id: Option<i64>,
}

/// Call out to CT to find all transponder IDs belonging to users contained in at least one of the
/// groups given by CT group ids.
async fn get_transponder_ids_in_group(
    config: &Config,
    group: &i64,
) -> Result<Vec<i64>, CTApiError> {
    let mut res = Vec::<i64>::new();
    let mut page = 0;
    let mut query_strings = [
        ("page", page.to_string()),
        // large limit to usually only make one request
        ("limit", "100".to_owned()),
        ("personFields[]", "transponderId".to_owned()),
    ];
    loop {
        page += 1;
        query_strings[0].1 = page.to_string();
        let response = match config
            .ct
            .client
            .get(format!(
                "https://{}/api/groups/{}/members",
                config.ct.host, group
            ))
            .query(&query_strings)
            .send()
            .await
        {
            Ok(x) => match x.text().await {
                Ok(text) => {
                    let deser_res: Result<CtGroupMemberResponse, _> = serde_json::from_str(&text);
                    match deser_res {
                        Ok(y) => y,
                        Err(e) => {
                            warn!("There was an error parsing the return value from CT: {e}");
                            warn!("The complete text received was: {text}");
                            return Err(CTApiError::Deserialize);
                        }
                    }
                }
                Err(e) => {
                    warn!("There was an error reading the response from CT as utf-8: {e}");
                    return Err(CTApiError::Utf8Decode);
                }
            },
            Err(e) => {
                warn!("There was a problem getting a response from CT");
                return Err(CTApiError::GetGroupMembers(e));
            }
        };
        if response.data.is_empty() {
            break;
        }
        res.extend(
            response
                .data
                .into_iter()
                .filter_map(|person| person.person_fields.transponder_id),
        );
    }
    Ok(res)
}

async fn get_transponder_ids_in_groups(
    config: &Config,
    groups: &[i64],
) -> Result<Vec<i64>, CTApiError> {
    futures::future::join_all(
        groups
            .iter()
            .map(|group| async move { get_transponder_ids_in_group(config, group).await }),
    )
    .await
    .into_iter()
    .flatten_ok()
    .collect::<Result<Vec<i64>, CTApiError>>()
}

#[derive(Debug, Deserialize)]
struct CtGetPersonResponse {
    data: PersonFields,
}

async fn get_transponder_id_of_user(
    config: &Config,
    created_by: i64,
) -> Result<Option<i64>, CTApiError> {
    match config
        .ct
        .client
        .get(format!(
            "https://{}/api/persons/{}",
            config.ct.host, created_by
        ))
        .send()
        .await
    {
        Ok(x) => match x.text().await {
            Ok(text) => {
                let deser_res: Result<CtGetPersonResponse, _> = serde_json::from_str(&text);
                match deser_res {
                    Ok(y) => Ok(y.data.transponder_id),
                    Err(e) => {
                        warn!("There was an error parsing the return value from CT: {e}");
                        warn!("The complete text received was: {text}");
                        Err(CTApiError::Deserialize)
                    }
                }
            }
            Err(e) => {
                warn!("There was an error reading the response from CT as utf-8: {e}");
                Err(CTApiError::Utf8Decode)
            }
        },
        Err(e) => {
            warn!("There was a problem getting a response from CT");
            Err(CTApiError::GetGroupMembers(e))
        }
    }
}

async fn get_permitted_transponders(
    config: &Config,
    created_by: i64,
    groups: &[i64],
) -> Result<Vec<i64>, CTApiError> {
    let mut transponders = get_transponder_ids_in_groups(config, groups).await?;
    tracing::debug!(
        "transponder ids from groupids {groups:?}: {:?}",
        transponders
    );
    if let Some(creator_transponder_id) = get_transponder_id_of_user(config, created_by).await? {
        transponders.push(creator_transponder_id);
    }
    Ok(transponders)
}

async fn get_raw_bookings(config: &Config) -> Result<CTBookingsResponse, CTApiError> {
    // we need to consider bookings from some time ago and some time in the future, because their prehold or posthold times
    // may overlap into today.
    let start_date = chrono::Utc::now().naive_utc() - config.global.posthold_time;
    // NOTE: CT will move to right-exclusive time intervals "at a future point in time". To be
    // save, we include one more day then we need here.
    let end_date =
        chrono::Utc::now().naive_utc() + config.global.prehold_time + chrono::TimeDelta::days(1);
    let mut query_strings = config
        .rooms
        .iter()
        .map(|room_config| room_config.ct_id)
        .map(|id| ("resource_ids[]", format!("{id}")))
        .collect::<Vec<_>>();
    query_strings.push((
        "from",
        <chrono::NaiveDateTime as Into<chrono::NaiveDate>>::into(start_date).to_string(),
    ));
    query_strings.push((
        "to",
        <chrono::NaiveDateTime as Into<chrono::NaiveDate>>::into(end_date).to_string(),
    ));
    // SECURITY
    // This gets all bookings that are pending or approved.
    // We accept that anyone can gain access by creating a booking request, even without that
    // request ever being approved.
    query_strings.push(("status_ids[]", "1".to_owned()));
    query_strings.push(("status_ids[]", "2".to_owned()));
    match config
        .ct
        .client
        .get(format!("https://{}/api/bookings", config.ct.host))
        .query(&query_strings)
        .send()
        .await
    {
        Ok(x) => match x.text().await {
            Ok(text) => {
                let deser_res: Result<CTBookingsResponse, _> = serde_json::from_str(&text);
                if let Ok(y) = deser_res {
                    Ok(y)
                } else {
                    warn!("There was an error parsing the return value from CT.");
                    warn!("The complete text received was: {text}");
                    Err(CTApiError::Deserialize)
                }
            }
            Err(e) => {
                warn!("There was an error reading the response from CT as utf-8: {e}");
                Err(CTApiError::Utf8Decode)
            }
        },
        Err(e) => {
            warn!("There was a problem getting a response from CT");
            Err(CTApiError::GetBookings(e))
        }
    }
}

/// Get all the relevant bookings from CT. This MAY include to many bookings (i.e. those whose
/// `prehold_time` or `posthold_time` have not yet started/ have already ended)
pub async fn get_relevant_bookings(config: &Config) -> Result<Vec<Booking>, CTApiError> {
    let response = get_raw_bookings(config).await?;

    futures::future::join_all(response.data.into_iter().map(|x: BookingsData| async move {
        // potentially change the start/end date to those of a calendar appointment if this
        // resource bookings was created from a calendar appointment
        let (start_date, end_date) = if let Some(AppointmentData {
            id: appointment_id,
            calendar_id,
        }) = x.base.appointment
        {
            let start_day = x
                .calculated
                .start_date
                .split('T')
                .next()
                .expect("Split always has a first element");
            let calendar_appointment =
                get_appointment(config, appointment_id, calendar_id, start_day).await?;
            (
                calendar_appointment.start_date,
                calendar_appointment.end_date,
            )
        } else {
            (x.calculated.start_date, x.calculated.end_date)
        };
        // we need to collect users permitted for this booking - first collect the groups
        // permitted from the description
        let permitted_groups = x
            .base
            .description
            .map(|descr| groups_from_description(&descr, &config.ct.group_magic_prefix))
            .unwrap_or_default();
        let permitted_transponders =
            get_permitted_transponders(config, x.base.meta.created_person.id, &permitted_groups)
                .await?;

        Ok::<Booking, CTApiError>(Booking {
            id: x.base.id,
            resource_id: x.base.resource_id,
            permitted_transponders,
            start_time: chrono::DateTime::parse_from_rfc3339(&start_date)
                // time can be Datetime or Date. Set datetime == start of day on all-day
                // events
                .or_else(|e| {
                    if chrono::format::ParseErrorKind::TooShort == e.kind() {
                        let naive = chrono::NaiveDate::parse_from_str(&start_date, "%Y-%m-%d")?;
                        Ok(chrono::DateTime::from_naive_utc_and_offset(
                            chrono::NaiveDateTime::new(
                                naive,
                                chrono::NaiveTime::from_hms_opt(0, 0, 0)
                                    .expect("statically good time"),
                            ),
                            chrono::FixedOffset::east_opt(0).expect("statically good offset"),
                        ))
                    } else {
                        Err(e)
                    }
                })
                .map_err(|e| CTApiError::ParseTime(e, start_date))?
                // we get the date from CT with an unknown offset, and need to cast to UTC
                // (actually, CT seems to always return UTC, but this is not part of a stably documented API)
                .into(),
            end_time: chrono::DateTime::parse_from_rfc3339(&end_date)
                // time can be Datetime or Date. Set datetime == end of day on all-day
                // events
                .or_else(|e| {
                    if chrono::format::ParseErrorKind::TooShort == e.kind() {
                        let naive = chrono::NaiveDate::parse_from_str(&end_date, "%Y-%m-%d")?;
                        Ok(chrono::DateTime::from_naive_utc_and_offset(
                            chrono::NaiveDateTime::new(
                                naive,
                                chrono::NaiveTime::from_hms_opt(23, 59, 59)
                                    .expect("statically good time"),
                            ),
                            chrono::FixedOffset::east_opt(0).expect("statically good offset"),
                        ))
                    } else {
                        Err(e)
                    }
                })
                .map_err(|e| CTApiError::ParseTime(e, end_date))?
                .into(),
        })
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
}

//! Everything directly interfacing with CT.

use std::collections::HashMap;

use reqwest::header;
use serde::Deserialize;
use tracing::warn;

use crate::{config::Config, Booking};

/// Create a Client with cookie store that sends the correct auth header each time
///
/// CT will honor the session cookie, and relogin when the cookie is stable because the correct
/// auth header is also sent.
pub fn create_client(login_token: &str) -> Result<reqwest::Client, reqwest::Error> {
    let mut headers = header::HeaderMap::new();
    headers.insert(header::ACCEPT, header::HeaderValue::from_static("application/json"));
    let mut auth_value = header::HeaderValue::from_str(&format!("Login {}", login_token)).expect("statically good header");
    auth_value.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, auth_value);
    reqwest::Client::builder().cookie_store(true).default_headers(headers).use_rustls_tls().build()
}



/// Something went wrong with CT
#[derive(Debug)]
pub enum CTApiError {
    GetBookings(reqwest::Error),
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
    let response = match config.ct_client
        .get(format!(
            "https://{}/api/calendars/{}/appointments/{}",
            config.ct.host, calendar_id, appointment_id
        ))
        .send()
        .await
    {
        Ok(x) => {
            let text_res = x.text().await;
            match text_res {
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
            }
        }
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

/// Get all the relevant bookings in the given timeframe.
pub async fn get_relevant_bookings(
    config: &Config,
    client: &reqwest::Client,
    start_date: chrono::NaiveDate,
    end_date: chrono::NaiveDate,
) -> Result<Vec<Booking>, CTApiError> {
    let mut query_strings = config
        .rooms
        .iter()
        .map(|room_config| room_config.ct_id)
        .map(|id| ("resource_ids[]", format!("{id}")))
        .collect::<Vec<_>>();
    query_strings.push(("from", start_date.to_string()));
    query_strings.push(("to", end_date.to_string()));
    // SECURITY
    // This gets all bookings that are pending or approved.
    // We accept that anyone can gain access by creating a booking request, even without that
    // request ever being approved.
    query_strings.push(("status_ids[]", "1".to_owned()));
    query_strings.push(("status_ids[]", "2".to_owned()));
    let response = match client
        .get(format!("https://{}/api/bookings", config.ct.host))
        .query(&query_strings)
        .send()
        .await
    {
        Ok(x) => {
            let text_res = x.text().await;
            match text_res {
                Ok(text) => {
                    let deser_res: Result<CTBookingsResponse, _> = serde_json::from_str(&text);
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
            }
        }
        Err(e) => {
            warn!("There was a problem getting a response from CT");
            return Err(CTApiError::GetBookings(e));
        }
    };
    futures::future::join_all(
        response
            .data
            .into_iter()
            .map(|x: BookingsData| async move {
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
                Ok::<Booking, CTApiError>(Booking {
                    id: x.base.id,
                    resource_id: x.base.resource_id,
                    created_by: x.base.meta.created_person.id,
                    description: x.base.description.unwrap_or_default(),
                    start_time: chrono::DateTime::parse_from_rfc3339(&start_date)
                        // time can be Datetime or Date. Set datetime == start of day on all-day
                        // events
                        .or_else(|e| {
                            if chrono::format::ParseErrorKind::TooShort == e.kind() {
                                let naive =
                                    chrono::NaiveDate::parse_from_str(&start_date, "%Y-%m-%d")?;
                                Ok(chrono::DateTime::from_naive_utc_and_offset(
                                    chrono::NaiveDateTime::new(
                                        naive,
                                        chrono::NaiveTime::from_hms_opt(0, 0, 0)
                                            .expect("statically good time"),
                                    ),
                                    chrono::FixedOffset::east_opt(0)
                                        .expect("statically good offset"),
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
                                let naive =
                                    chrono::NaiveDate::parse_from_str(&end_date, "%Y-%m-%d")?;
                                Ok(chrono::DateTime::from_naive_utc_and_offset(
                                    chrono::NaiveDateTime::new(
                                        naive,
                                        chrono::NaiveTime::from_hms_opt(23, 59, 59)
                                            .expect("statically good time"),
                                    ),
                                    chrono::FixedOffset::east_opt(0)
                                        .expect("statically good offset"),
                                ))
                            } else {
                                Err(e)
                            }
                        })
                        .map_err(|e| CTApiError::ParseTime(e, end_date))?
                        .into(),
                })
            }),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
}


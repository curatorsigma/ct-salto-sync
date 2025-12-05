//! Everything for communication with the Salto API.
//!
//! NOTES:
//! 1. This API is completely undocumented and I reverse engineered it. However, I do not know of
//!    any other way to get the ExtId for a User, so I had to do this.
//! 2. The actual handover of data into salto happens via the official staging table and is
//!    implemented in [`crate::write_staging`].

use std::{collections::HashMap, pin::Pin, sync::Arc, task::Poll};

use base64::{Engine, prelude::BASE64_STANDARD};
use futures::{StreamExt, TryStreamExt};
use rand::RngCore;
use reqwest::header;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, trace, warn};

use crate::config::{Config, SaltoConfigData};

#[derive(Debug)]
pub enum SaltoApiError {
    Utf8Decode,
    DeserializeDirect(serde_json::Error),
    DeserializeReqwest(reqwest::Error),
    NoResponse(reqwest::Error),
    CannotCreateClient(reqwest::Error),
    CannotGetUsers(reqwest::Error),
    ClientBuilder(reqwest::Error),
}
impl core::fmt::Display for SaltoApiError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::Utf8Decode => {
                write!(f, "Unable to decode response body as utf-8.")
            }
            Self::DeserializeDirect(e) => {
                write!(
                    f,
                    "Unable to deserialize response as the expected struct: {e}."
                )
            }
            Self::DeserializeReqwest(e) => {
                write!(
                    f,
                    "Unable to deserialize response as the expected struct: {e}."
                )
            }
            Self::NoResponse(e) => {
                write!(f, "Did not get a postive response from salto: {e}.")
            }
            Self::CannotCreateClient(e) => {
                write!(
                    f,
                    "Unable to create a reqwest client for use with salto bearer auth: {e}."
                )
            }
            Self::CannotGetUsers(e) => {
                write!(f, "Unable to get users from Salto: {e}.")
            }
            Self::ClientBuilder(e) => {
                write!(f, "Unable to create initial client for oauth login to salto: {e}.")
            }
        }
    }
}
impl core::error::Error for SaltoApiError {}

#[derive(Deserialize, Debug, Clone)]
struct SaltoUser {
    #[serde(rename = "ExtId")]
    ext_id: String,
    #[serde(rename = "Title", deserialize_with = "deserialize_transponder_id_from_title")]
    transponder_id: i64,
}

fn deserialize_transponder_id_from_title<'de, D>(
    deserializer: D,
) -> Result<i64, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let as_string: String = serde::de::Deserialize::deserialize(deserializer)?;
    as_string.parse::<i64>().map_err(serde::de::Error::custom)
}

/// Generate a non-repeating 32 byte salt
///
/// NOTE:
/// saltos webapp only uses 32 random bytes without guaranteeing non-repeating salts
fn salto_salt() -> String {
    let mut raw_bytes = [0_u8; 32];
    // current time since the epoch to prevent salt reuse
    let now_in_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("Should be after the epoch");
    raw_bytes[0..=7].clone_from_slice(&now_in_secs.as_secs().to_le_bytes());
    raw_bytes[8..=11].clone_from_slice(&now_in_secs.subsec_millis().to_le_bytes());
    // 8 bytes against predictability
    rand::rngs::ThreadRng::default().fill_bytes(&mut raw_bytes[12..=31]);
    hex::encode(raw_bytes)
}

/// Calcualte the Salto-style password hash.
///
/// This is defined as:
/// <SALT><SHA256(salt + password)>,
/// where SALT is 32 random bytes in hex-coding.
fn salto_password_hash(password: &str) -> String {
    // 32 byte = 64 chars in hex for the salt, 32 byte = 64 chars in hex for the sha256-sum
    let mut complete_hash = String::with_capacity(128);
    let salto_salt = salto_salt();
    let mut hasher = Sha256::new();
    hasher.update(salto_salt.as_bytes());
    hasher.update(password.as_bytes());
    complete_hash.push_str(&salto_salt);
    complete_hash.push_str(&hex::encode(hasher.finalize()));
    complete_hash
}

#[derive(Debug, Deserialize)]
struct AuthorizationTokenResponse {
    access_token: String,
}
/// Log in to salto and return the access_token gotten from the Oauth endpoint
async fn salto_login(config: &SaltoConfigData) -> Result<String, SaltoApiError> {
    let mut form_data = HashMap::new();
    form_data.insert("grant_type", "password");
    form_data.insert("client_id", "webapp");
    form_data.insert("scope", "offline_access global");
    // look, i did not design this API, ok??
    let username_as_base64 = BASE64_STANDARD.encode(&config.username);
    form_data.insert("username", &username_as_base64);
    let hash = salto_password_hash(&config.password);
    form_data.insert("password", &hash);
    Ok(
        match reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .use_rustls_tls()
            .build()
            .map_err(SaltoApiError::ClientBuilder)?
            .post(format!("{}/oauth/connect/token", config.base_url))
            .form(&form_data)
            .query(&form_data)
            .header(reqwest::header::CONTENT_LENGTH, 222)
            .send()
            .await
        {
            Ok(x) => {
                let text_res = x.text().await;
                match text_res {
                    Ok(text) => {
                        let deser_res: Result<AuthorizationTokenResponse, _> =
                            serde_json::from_str(&text);
                        match deser_res {
                            Ok(y) => y.access_token,
                            Err(e) => {
                                return Err(SaltoApiError::DeserializeDirect(e));
                            }
                        }
                    }
                    Err(_e) => {
                        return Err(SaltoApiError::Utf8Decode);
                    }
                }
            }
            Err(e) => {
                return Err(SaltoApiError::NoResponse(e));
            }
        },
    )
}

pub async fn create_client(config: &SaltoConfigData) -> Result<reqwest::Client, SaltoApiError> {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );
    let access_token = salto_login(config).await?;
    let mut auth_value = header::HeaderValue::from_str(&format!("Bearer {}", access_token))
        .expect("statically good header");
    auth_value.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, auth_value);
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .cookie_store(true)
        .default_headers(headers)
        .use_rustls_tls()
        .build()
        .map_err(SaltoApiError::CannotCreateClient)
}

#[derive(Debug, Deserialize)]
struct GetUserListStartingFromItemResponse {
    /// The users ExtId
    #[serde(rename = "ExtId")]
    ext_id: String,
    /// Contains the users transponder id, as a string, potentially with zeros prefixed
    #[serde(rename = "Title")]
    title: String,
}

/// Names are hard.
///
/// The Form data we need to pass to get the next page of users from Saltos api.
#[derive(Debug, Serialize)]
struct SaltoGetUserListStartingFromItemRequestData {
    #[serde(rename = "startingItem")]
    starting_item: Option<serde_json::Value>,
    #[serde(rename = "orderBy")]
    order_by: i32,
    #[serde(rename = "maxCount")]
    max_count: i32,
    #[serde(rename = "returnRelations")]
    return_relations: SaltoGetUserListStartingFromItemRequestDataReturnRelations,
    #[serde(rename = "filterCriteria")]
    filter_criteria: String,
    #[serde(rename = "isForward")]
    is_forward: bool,
}
impl Default for SaltoGetUserListStartingFromItemRequestData {
    fn default() -> Self {
        Self::new_from_last_item(None)
    }
}
impl SaltoGetUserListStartingFromItemRequestData {
    fn new_from_last_item(last: Option<serde_json::Value>) -> Self {
        Self {
            starting_item: last,
            order_by: 0,
            max_count: 21,
            return_relations: SaltoGetUserListStartingFromItemRequestDataReturnRelations::default(),
            filter_criteria: "".to_string(),
            is_forward: false,
        }
    }
}

#[derive(Debug, Serialize)]
struct SaltoGetUserListStartingFromItemRequestDataReturnRelations {
    #[serde(rename = "$type")]
    relation_type: String,
    #[serde(rename = "Data")]
    data: bool,
    #[serde(rename = "Enrollment")]
    enrollment: bool,
}
impl Default for SaltoGetUserListStartingFromItemRequestDataReturnRelations {
    fn default() -> Self {
        Self {
            relation_type: "Salto.Services.Web.Model.Dto.Cardholders.Users.UserRelationSet"
                .to_string(),
            data: false,
            enrollment: false,
        }
    }
}

/// Get the requested page of users from salto
///
/// Assumes that the client is logged in. Requires the full return value that ended the last page.
async fn get_next_salto_user_page(
    last_page_end: Option<serde_json::Value>,
    config: Arc<Config>,
) -> Result<std::vec::IntoIter<serde_json::Value>, SaltoApiError> {
    let formdata = SaltoGetUserListStartingFromItemRequestData::new_from_last_item(last_page_end);
    match config
        .salto
        .client
        .post(format!(
            "{}/rpc/GetUserListStartingFromItem",
            config.salto.base_url
        ))
        .json(&formdata)
        .send()
        .await
    {
        Ok(x) => Ok(x
            .json::<Vec<serde_json::Value>>()
            .await
            .map_err(SaltoApiError::DeserializeReqwest)?
            .into_iter()),
        Err(e) => {
            warn!("Failed to get a page of users from Salto: {e}");
            Err(SaltoApiError::CannotGetUsers(e))
        }
    }
}

/// Streams all Salto Users from saltos RPC API.
///
/// NOTE:
/// When the calls to salto fail, there may be an infinite number of retries with the same request,
/// leading to the same error. The consumer should handle errors apropriately and potentially
/// short-circuit on the first (or the first repeated) error.
struct SaltoUserStream {
    config: Arc<Config>,
    last_page_full_last_entry: Option<serde_json::Value>,
    /// Users present on last page - will iterate these to the end before requesting the next page
    on_last_page: Box<dyn ExactSizeIterator<Item = Result<SaltoUser, SaltoApiError>> + Send>,
    current_future: Option<
        Pin<
            Box<
                dyn futures::future::Future<
                        Output = Result<std::vec::IntoIter<serde_json::Value>, SaltoApiError>,
                    > + Send,
            >,
        >,
    >,
}
impl SaltoUserStream {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            last_page_full_last_entry: None,
            on_last_page: Box::new(vec![].into_iter()),
            current_future: None,
        }
    }
}
impl tokio_stream::Stream for SaltoUserStream {
    type Item = Result<SaltoUser, SaltoApiError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        // we still have data (sync)
        if let Some(next_user) = self.on_last_page.next() {
            return std::task::Poll::Ready(Some(next_user));
        };

        // we have the next future already queued; keep polling it
        if self.current_future.is_none() {
            let our_config = self.config.clone();
            self.current_future = Some(Box::pin(get_next_salto_user_page(
                self.last_page_full_last_entry.clone(),
                our_config,
            )));
        };

        match self.current_future.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Pending => {
                return Poll::Pending;
            }
            Poll::Ready(result) => {
                self.current_future = None;
                match result {
                    Ok(next_page) => {
                        if let Some(last_entry_ref) = next_page.as_slice().last() {
                            self.last_page_full_last_entry = Some(last_entry_ref.clone());
                            self.on_last_page = Box::new(next_page.map(|val| {
                                serde_json::from_value::<SaltoUser>(val)
                                    .map_err(SaltoApiError::DeserializeDirect)
                            }));
                            self.current_future = None;
                            return Poll::Ready(Some(
                                self.on_last_page
                                    .next()
                                    .expect("checked that the next page contains entries"),
                            ));
                        } else {
                            return Poll::Ready(None);
                        }
                    }
                    Err(e) => {
                        return Poll::Ready(Some(Err(e)));
                    }
                }
            }
        }
    }
}

/// Try to find the ExtId for each transponder
///
/// # Errors
/// Returns an Error when an API call fails.
/// When no ExtId is found for a user, inserts None into the HashMap
pub async fn get_ext_ids_by_transponder<'a, I: Iterator<Item = &'a i64>>(
    config: Arc<Config>,
    transponders: I,
) -> Result<HashMap<i64, Option<String>>, SaltoApiError> {
    let mut res: HashMap<i64, Option<String>> = transponders
        .map(|transponder| (*transponder, None))
        .collect();
    let mut users = SaltoUserStream::new(config).into_stream();
    while let Some(user_res) = users.next().await {
        match user_res {
            Err(SaltoApiError::DeserializeDirect(e)) => {
                trace!("Failed to deserialize user object completely. Skipping this user: {e}.");
            }
            Ok(user) => {
                res.entry(user.transponder_id)
                    .and_modify(|value| *value = Some(user.ext_id));
            }
            Err(e) => {
                warn!("Failed to get next user from salto: {e}");
                return Err(e);
            }
        }
    }
    Ok(res)
}

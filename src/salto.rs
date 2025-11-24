//! Everything for communication with the Salto API.
//!
//! NOTES:
//! 1. This API is completely undocumented and I reverse engineered it. However, I do not know of
//!    any other way to get the ExtId for a User, so I had to do this.
//! 2. The actual handover of data into salto happens via the official staging table and is
//!    implemented in [`crate::write_staging`].

use std::{collections::HashMap, sync::Arc};

use base64::{prelude::BASE64_STANDARD, Engine};
use rand::RngCore;
use reqwest::header;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::config::{Config, SaltoConfigData};

#[derive(Debug)]
pub enum SaltoApiError {
    Utf8Decode,
    Deserialize,
    NoResponse(reqwest::Error),
    CannotCreateClient(reqwest::Error),
    CannotGetUsers(reqwest::Error),
}
impl core::fmt::Display for SaltoApiError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::Utf8Decode => {
                write!(f, "Unable to decode response body as utf-8.")
            }
            Self::Deserialize => {
                write!(f, "Unable to deserialize response as the expected struct.")
            }
            Self::NoResponse(e) => {
                write!(f, "Did not get a postive response from salto: {e}.")
            }
            Self::CannotCreateClient(e) => {
                write!(f, "Unable to create a reqwest client for use with salto bearer auth: {e}.")
            }
            Self::CannotGetUsers(e) => {
                write!(f, "Unable to get users from Salto: {e}.")
            }
        }
    }
}
impl core::error::Error for SaltoApiError {}

struct SaltoUser {
    ext_id: String,
    transponder_id: i64,
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
    authorization_token: String,
}
/// Log in to salto and return the authorization_token gotten from the Oauth endpoint
async fn salto_login(config: &SaltoConfigData,) -> Result<String, SaltoApiError> {
    let mut form_data = HashMap::new();
    form_data.insert("grant_type", "password");
    form_data.insert("client_id", "webapp");
    form_data.insert("scope", "offline_access+global");
    // look, i did not design this API, ok??
    let username_as_base64 = BASE64_STANDARD.encode(&config.username);
    form_data.insert("username", &username_as_base64);
    let hash = salto_password_hash(&config.password);
    form_data.insert("password", &hash);
    Ok(match reqwest::Client::new().post(format!("{}/oauth/connect/token", config.base_url)).json(&form_data).send().await {
        Ok(x) => {
            let text_res = x.text().await;
            match text_res {
                Ok(text) => {
                    let deser_res: Result<AuthorizationTokenResponse, _> = serde_json::from_str(&text);
                    if let Ok(y) = deser_res {
                        y.authorization_token
                    } else {
                        warn!("There was an error parsing the return value from salto.");
                        warn!("The complete text received was: {text}");
                        return Err(SaltoApiError::Deserialize);
                    }
                }
                Err(e) => {
                    warn!("There was an error reading the response from salto as utf-8: {e}");
                    return Err(SaltoApiError::Utf8Decode);
                }
            }
        }
        Err(e) => {
            warn!("There was a problem getting a response from Salto");
            return Err(SaltoApiError::NoResponse(e));
        }
    })
}

pub async fn create_client(config: &SaltoConfigData,) -> Result<reqwest::Client, SaltoApiError> {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );
    let authorization_token = salto_login(config).await?;
    let mut auth_value = header::HeaderValue::from_str(&format!("Bearer {}", authorization_token))
        .expect("statically good header");
    auth_value.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, auth_value);
    reqwest::Client::builder()
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
        Self { starting_item: last, order_by: 0, max_count: 21, return_relations: SaltoGetUserListStartingFromItemRequestDataReturnRelations::default(), filter_criteria: "".to_string(), is_forward: false }
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
        Self { relation_type: "Salto.Services.Web.Model.Dto.Cardholders.Users.UserRelationSet".to_string(), data: false, enrollment: false}
    }
}

/// Get the requested page of users from salto
///
/// Assumes that the client is logged in. Requires the full return value that ended the last page.
async fn get_next_salto_user_page(last_page_end: Option<serde_json::Value>, config: &Config,) -> Result<impl Iterator<Item = serde_json::Value>, SaltoApiError> {
    let formdata = SaltoGetUserListStartingFromItemRequestData::new_from_last_item(last_page_end);
    match config.salto.client.post(format!("{}/rpc/GetUserListStartingFromItem", config.salto.base_url)).json(&formdata).send().await {
        Ok(x) => {
            Ok(x.json::<Vec<serde_json::Value>>().await.map_err(|_e| SaltoApiError::Deserialize)?.into_iter())
        }
        Err(e) => {
            warn!("Failed to get a page of users from Salto: {e}");
            Err(SaltoApiError::CannotGetUsers(e))
        }
    }
}

struct SaltoUserStream {
    config: Arc<Config>,
    last_page_full_last_entry: Option<serde_json::Value>,
    /// Users present on last page - will iterate these to the end before requesting the next page
    on_last_page: Option<Box<dyn Iterator<Item = SaltoUser>>>,
    current_future: Option<Box<dyn futures::future::Future<Output = Result<serde_json::Value, SaltoApiError>>>>,
}
impl SaltoUserStream {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            last_page_full_last_entry: None,
            on_last_page: None,
            current_future: None,
        }
    }
}
impl tokio_stream::Stream for SaltoUserStream {
    type Item = Result<SaltoUser, SaltoApiError>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        if let Some(next) = self.on_last_page.and_then(|iter| iter.next()) {
            return std::task::Poll::Ready(Some(Ok(next)))
        };
        // the last page is empty, we need to get the next one
        // call get_next_salto_user_page, update our state and return the right element, if
        // possible
        // if a return to get_next_salto_user_page is empty, return Ready(None)
    }
}


/// Try to find all the ExtId for each transponder
///
/// # Errors
/// Returns an Error when an API call fails.
/// When no ExtId is found for a user, inserts None into the HashMap
pub async fn get_ext_ids_by_transponder<'a, I: Iterator<Item = &'a i64>>(
    config: &Config,
    transponders: I,
) -> Result<HashMap<i64, Option<String>>, SaltoApiError> {
    let mut res: HashMap<i64, Option<String>> = transponders.map(|transponder| (*transponder, None)).collect();
    // iterate over the salto user list page by page
    // when we know a transponder, add the relevant ExtId into the map
    todo!();
    Ok(res)
}

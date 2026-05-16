use derive_more::From;

use crate::salto::SaltoApiError;

pub type Res<T> = core::result::Result<T, AppErr>;

#[derive(Debug, From)]
pub enum AppErr {
    Msg(&'static str),
    IO(String, std::io::Error),

    ChurchToolsError(reqwest::Error),
    SaltoError(SaltoApiError),

    #[from]
    ConfigParsingError(serde_yaml::Error),
    #[from]
    DbConnectionError(sqlx::Error),
}

impl core::fmt::Display for AppErr {
    fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::result::Result<(), core::fmt::Error> {
        write!(fmt, "{self:?}")
    }
}

impl core::error::Error for AppErr {}

//! All the db-related functions

#[derive(Debug)]
pub enum DBError {
    SelectBookings(sqlx::Error),
    InsertBooking(sqlx::Error),
    DeleteBooking(sqlx::Error),
    UpdateBooking(sqlx::Error),
}
impl std::fmt::Display for DBError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::SelectBookings(e) => {
                write!(
                    f,
                    "Unable to select bookings from the DB. Inner Error: {e}."
                )
            }
            Self::InsertBooking(e) => {
                write!(f, "Unable to insert booking into the DB. Inner Error: {e}.")
            }
            Self::UpdateBooking(e) => {
                write!(f, "Unable to update booking in the DB. Inner Error: {e}.")
            }
            Self::DeleteBooking(e) => {
                write!(f, "Unable to delete booking from the DB. Inner Error: {e}.")
            }
        }
    }
}
impl std::error::Error for DBError {}

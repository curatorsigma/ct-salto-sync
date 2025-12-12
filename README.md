# Churchtools -> Salto Sync
This service pulls booking data from Churchtools and pushes it to a Salto Staging table.

# What is synced?
Each booking for a room given in the config file will be read.
The user which created the booking will gain access to the zone associated to the room for the time of the booking.
You may specify more Groups that also gain access. For the example config, you could allow all users in the group with churchtools id `123` by adding `SALTO_ALLOW_123` to the bookings comments.

# Important Notes:
To identify users between churchtools and salto, we make use of these requirements:
- Users in churchtools must have `transponderId` set to the `title` in salto, and this must be parsable as i64.
- We need to read the user list in Salto to find the ExtID. This uses an undocumented rpc-API in Salto I reverse engineered. See `src/salto.rs`.

# LICENSE
This project is licensed under MIT-0 (MIT No Attribution). By contributing to this repositry, you agree that your code will be licensed as MIT-0.

For my rationale for using MIT-0 instead of another more common license, please see https://copy.church/objections/attribution/#why-not-require-attribution .

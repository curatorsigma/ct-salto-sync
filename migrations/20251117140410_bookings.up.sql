CREATE TABLE bookings (
	id INTEGER PRIMARY KEY,
	resource_id INTEGER NOT NULL,
	start_time DATETIME NOT NULL,
	end_time DATETIME NOT NULL,
	description TEXT NOT NULL,
	created_by INTEGER NOT NULL
);


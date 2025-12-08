CREATE TABLE salto_staging (
	id INTEGER PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
	ExtID TEXT UNIQUE NOT NULL,
	ExtZoneIDList TEXT NOT NULL,
	Action INTEGER NOT NULL DEFAULT 2,
	-- 1: has to be processed
	-- 0: was already processed
	ToBeProcessedBySalto INTEGER NOT NULL DEFAULT 1,
	ProcessedDateTime TIMESTAMP,
	ErrorCode INTEGER,
	ErrorMessage TEXT
);

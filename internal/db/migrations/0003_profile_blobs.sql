-- A profile file's contents are kept in the profiles bucket as
-- <user>/<path>, in the version version_id names, with their size here for
-- the profile's limit. data holds the contents of files written before,
-- until the server moves them out (profile.Store.MoveToBlobs); a removed
-- file has neither.
ALTER TABLE profile_files
    ALTER COLUMN data DROP NOT NULL,
    ADD COLUMN version_id text,
    ADD COLUMN size bigint NOT NULL DEFAULT 0;

UPDATE profile_files SET size = length(data) WHERE NOT deleted;

-- Profile files' contents are in the blob store. A server that never moved
-- them out (profile.Store.MoveToBlobs, in releases from before this one)
-- still has some here, and dropping them would lose them: it stops instead.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM profile_files WHERE data IS NOT NULL) THEN
        RAISE EXCEPTION 'profile files are still in the database: start the release before this one once, so it moves them into the object store, then this one';
    END IF;
END $$;

ALTER TABLE profile_files DROP COLUMN data;

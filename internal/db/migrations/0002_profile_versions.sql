-- The last version given to each user's profile files. A version is taken
-- from here, not from the files: unsharing a path deletes its files, and the
-- next version must still be past every one a session has sent, the deleted
-- ones' included.
CREATE TABLE profile_versions (
    user_id uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    version bigint NOT NULL
);

INSERT INTO profile_versions (user_id, version)
SELECT user_id, max(version) FROM profile_files GROUP BY user_id;

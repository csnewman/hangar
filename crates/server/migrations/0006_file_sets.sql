-- A pack is a named set of files that templates give the environments made
-- from them: .env files for every template of a project, say. It belongs to
-- a person or a team, as an image repository does. Its paths are absolute,
-- a directory's ending in a slash. A shared pack is one copy of its files
-- for everyone who uses it; a personal one is each person's own copy.
CREATE TABLE file_packs (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text NOT NULL,
    owner_id    uuid REFERENCES users (id) ON DELETE CASCADE,
    team_id     uuid REFERENCES teams (id) ON DELETE CASCADE,
    CHECK ((owner_id IS NULL) <> (team_id IS NULL)),
    personal    boolean NOT NULL DEFAULT false,
    description text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX file_packs_name ON file_packs (coalesce(owner_id, team_id), lower(name));

CREATE TABLE file_pack_paths (
    pack_id uuid NOT NULL REFERENCES file_packs (id) ON DELETE CASCADE,
    path    text NOT NULL,
    PRIMARY KEY (pack_id, path)
);

-- A file set is one copy of files kept in step with environments: a user's
-- profile, whose id is the user's, a shared pack's files, or a person's own
-- copy of a personal pack's.
CREATE TABLE file_sets (
    id      uuid PRIMARY KEY,
    user_id uuid REFERENCES users (id) ON DELETE CASCADE,
    pack_id uuid REFERENCES file_packs (id) ON DELETE CASCADE,
    CHECK (user_id IS NOT NULL OR pack_id IS NOT NULL)
);

CREATE UNIQUE INDEX file_sets_owner ON file_sets (coalesce(user_id, '00000000-0000-0000-0000-000000000000'),
    coalesce(pack_id, '00000000-0000-0000-0000-000000000000'));

INSERT INTO file_sets (id, user_id) SELECT id, id FROM users;

-- What was each user's profile is their profile's set.
ALTER TABLE profile_files RENAME TO set_files;
ALTER TABLE set_files RENAME COLUMN user_id TO set_id;
ALTER TABLE set_files DROP CONSTRAINT profile_files_user_id_fkey;
ALTER TABLE set_files ADD FOREIGN KEY (set_id) REFERENCES file_sets (id) ON DELETE CASCADE;

ALTER TABLE profile_versions RENAME TO set_versions;
ALTER TABLE set_versions RENAME COLUMN user_id TO set_id;
ALTER TABLE set_versions DROP CONSTRAINT profile_versions_user_id_fkey;
ALTER TABLE set_versions ADD FOREIGN KEY (set_id) REFERENCES file_sets (id) ON DELETE CASCADE;

ALTER TABLE profile_locks RENAME TO set_locks;
ALTER TABLE set_locks RENAME COLUMN user_id TO set_id;
ALTER TABLE set_locks DROP CONSTRAINT profile_locks_user_id_fkey;
ALTER TABLE set_locks ADD FOREIGN KEY (set_id) REFERENCES file_sets (id) ON DELETE CASCADE;

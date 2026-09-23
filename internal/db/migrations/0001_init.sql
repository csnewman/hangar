CREATE TABLE users (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    username      text NOT NULL,
    display_name  text NOT NULL DEFAULT '',
    -- An argon2id hash in PHC string form. NULL for a user who signs in only
    -- through an external identity provider and has no local password.
    password_hash text,
    is_admin      boolean NOT NULL DEFAULT false,
    -- A disabled user cannot sign in, and their sessions stop working. Their
    -- environments are kept, so disabling is reversible.
    disabled_at   timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Usernames compare case-insensitively, so "Alice" and "alice" are one user.
CREATE UNIQUE INDEX users_username ON users (lower(username));

CREATE TABLE sessions (
    -- SHA-256 of the token in the cookie. The token itself is never stored,
    -- so a copy of this table cannot be used to sign in.
    token_hash   bytea PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL
);

CREATE INDEX sessions_user ON sessions (user_id);

CREATE TABLE workers (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name            text NOT NULL UNIQUE,
    credential_hash bytea NOT NULL,
    labels          jsonb NOT NULL DEFAULT '{}',
    cpus            integer NOT NULL DEFAULT 0,
    memory_mib      integer NOT NULL DEFAULT 0,
    -- Environments the worker reports that have no row here.
    unknown         jsonb NOT NULL DEFAULT '[]',
    -- Bumped whenever the worker's desired set changes, in the same
    -- transaction as the change, so a waiting worker can tell whether it has
    -- seen the latest.
    desired_version bigint NOT NULL DEFAULT 1,
    last_seen_at    timestamptz,
    revoked_at      timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE environments (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- A user who owns environments cannot be deleted; disable them instead,
    -- or delete their environments first.
    owner_id    uuid NOT NULL REFERENCES users (id),
    name        text NOT NULL,
    image       text NOT NULL,
    cpus        integer NOT NULL CHECK (cpus > 0),
    memory_mib  integer NOT NULL CHECK (memory_mib > 0),
    desired     text NOT NULL CHECK (desired IN ('running', 'stopped', 'deleted')),
    -- An environment's disks are local to the worker it was placed on, so
    -- placement is permanent. Moving it would lose them.
    worker_id   uuid REFERENCES workers (id),
    phase       text NOT NULL DEFAULT 'pending',
    reason      text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- The placement queue: environments that want to run and have nowhere to.
CREATE INDEX environments_unplaced ON environments (created_at)
    WHERE worker_id IS NULL AND desired = 'running';

CREATE INDEX environments_worker ON environments (worker_id);

-- Names are a user's own: two users may each have an environment called "dev".
CREATE UNIQUE INDEX environments_owner_name ON environments (owner_id, name);

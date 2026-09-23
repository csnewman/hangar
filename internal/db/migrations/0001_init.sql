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
    name        text NOT NULL UNIQUE,
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

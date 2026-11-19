-- The SQLite store's schema: the Postgres tables of migrations 0001-0024,
-- column for column. Types as SQLite keeps them: UUIDs and bytes BLOB,
-- times TEXT (RFC 3339, UTC, as sqlx writes them, so they sort as text),
-- JSON and arrays TEXT (a JSON array), booleans INTEGER. Environments'
-- revisions are the store's (`environment_revisions`), given at commit.

CREATE TABLE users (
    id            BLOB    NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    username      TEXT    NOT NULL,
    display_name  TEXT    NOT NULL DEFAULT '',
    password_hash TEXT,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    kind          TEXT    NOT NULL DEFAULT 'person' CHECK (kind IN ('person', 'system')),
    disabled_at   TEXT,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    theme         TEXT    NOT NULL DEFAULT 'system' CHECK (theme IN ('system', 'light', 'dark'))
) STRICT;
CREATE UNIQUE INDEX users_username ON users (lower(username));

CREATE TABLE sessions (
    token_hash   BLOB NOT NULL PRIMARY KEY,
    user_id      BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    last_seen_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    expires_at   TEXT NOT NULL
) STRICT;
CREATE INDEX sessions_user ON sessions (user_id);

CREATE TABLE access_tokens (
    id           BLOB NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    user_id      BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    token_hash   BLOB NOT NULL UNIQUE,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    last_used_at TEXT,
    expires_at   TEXT
) STRICT;
CREATE INDEX access_tokens_user ON access_tokens (user_id);

CREATE TABLE workers (
    id              BLOB    NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    name            TEXT    NOT NULL UNIQUE,
    credential_hash BLOB    NOT NULL,
    labels          TEXT    NOT NULL DEFAULT '{}',
    cpus            INTEGER NOT NULL DEFAULT 0,
    memory_mib      INTEGER NOT NULL DEFAULT 0,
    unknown         TEXT    NOT NULL DEFAULT '[]',
    stats           TEXT,
    images          TEXT    NOT NULL DEFAULT '[]',
    gpu             TEXT,
    desired_version INTEGER NOT NULL DEFAULT 1,
    last_seen_at    TEXT,
    revoked_at      TEXT,
    created_at      TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    monitor         TEXT    NOT NULL DEFAULT '',
    version         TEXT    NOT NULL DEFAULT '',
    features        TEXT    NOT NULL DEFAULT '[]'
) STRICT;

CREATE TABLE audit_events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    actor_user   BLOB REFERENCES users (id) ON DELETE SET NULL,
    actor_worker BLOB REFERENCES workers (id) ON DELETE SET NULL,
    actor_name   TEXT NOT NULL,
    source_ip    TEXT NOT NULL DEFAULT '',
    action       TEXT NOT NULL,
    target_type  TEXT NOT NULL,
    target_id    TEXT NOT NULL,
    target_name  TEXT NOT NULL DEFAULT '',
    subjects     TEXT NOT NULL,
    details      TEXT NOT NULL DEFAULT '{}'
) STRICT;

CREATE TABLE teams (
    id          BLOB NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    slug        TEXT NOT NULL,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'))
) STRICT;
CREATE UNIQUE INDEX teams_slug ON teams (lower(slug));

CREATE TABLE team_members (
    team_id BLOB NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role    TEXT NOT NULL CHECK (role IN ('viewer', 'member', 'admin')),
    PRIMARY KEY (team_id, user_id)
) STRICT;
CREATE INDEX team_members_user ON team_members (user_id);

CREATE TABLE templates (
    id          BLOB    NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    owner_id    BLOB    NOT NULL REFERENCES users (id),
    team_id     BLOB    REFERENCES teams (id),
    name        TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    visibility  TEXT    NOT NULL CHECK (visibility IN ('private', 'shared')),
    spec        TEXT    NOT NULL,
    revision    INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'))
) STRICT;
CREATE UNIQUE INDEX templates_owner_name ON templates (owner_id, name) WHERE team_id IS NULL;
CREATE UNIQUE INDEX templates_team_name ON templates (team_id, name) WHERE team_id IS NOT NULL;

CREATE TABLE template_collaborators (
    template_id BLOB NOT NULL REFERENCES templates (id) ON DELETE CASCADE,
    user_id     BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (template_id, user_id)
) STRICT;
CREATE INDEX template_collaborators_user ON template_collaborators (user_id);

CREATE TABLE template_team_collaborators (
    template_id BLOB NOT NULL REFERENCES templates (id) ON DELETE CASCADE,
    team_id     BLOB NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    PRIMARY KEY (template_id, team_id)
) STRICT;
CREATE INDEX template_team_collaborators_team ON template_team_collaborators (team_id);

CREATE TABLE environments (
    id                  BLOB    NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    short_id            TEXT    NOT NULL UNIQUE
        CHECK (length(short_id) = 6 AND short_id NOT GLOB '*[^a-z0-9]*'),
    owner_id            BLOB    NOT NULL REFERENCES users (id),
    name                TEXT    NOT NULL,
    template_id         BLOB    REFERENCES templates (id) ON DELETE SET NULL,
    template_name       TEXT    NOT NULL,
    template_revision   INTEGER NOT NULL DEFAULT 1,
    spec                TEXT    NOT NULL,
    image               TEXT    NOT NULL,
    image_digest        TEXT    NOT NULL DEFAULT '',
    image_pin_want      TEXT,
    image_drop_rollback INTEGER NOT NULL DEFAULT 0,
    image_update        TEXT,
    image_rollback      TEXT,
    ports_public        INTEGER NOT NULL DEFAULT 0,
    cpus                INTEGER NOT NULL CHECK (cpus > 0),
    memory_mib          INTEGER NOT NULL CHECK (memory_mib > 0),
    desired             TEXT    NOT NULL CHECK (desired IN ('running', 'stopped', 'suspended', 'deleted')),
    worker_id           BLOB    REFERENCES workers (id),
    phase               TEXT    NOT NULL DEFAULT 'pending',
    reason              TEXT    NOT NULL DEFAULT '',
    stats               TEXT,
    progress            TEXT,
    created_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    updated_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    guest_agent         TEXT,
    revision            INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE UNIQUE INDEX environments_owner_name ON environments (owner_id, lower(name));
CREATE INDEX environments_unplaced ON environments (created_at)
    WHERE worker_id IS NULL AND desired = 'running';
CREATE INDEX environments_worker ON environments (worker_id);

-- The last revision an environment was given: one row.
CREATE TABLE environment_revisions (
    last INTEGER NOT NULL
) STRICT;
INSERT INTO environment_revisions (last) VALUES (0);

CREATE TABLE worker_image_removals (
    worker_id    BLOB NOT NULL REFERENCES workers (id) ON DELETE CASCADE,
    ref          TEXT NOT NULL,
    digest       TEXT NOT NULL,
    requested_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    PRIMARY KEY (worker_id, ref, digest)
) STRICT;

CREATE TABLE worker_environment_removals (
    worker_id    BLOB NOT NULL REFERENCES workers (id) ON DELETE CASCADE,
    environment  TEXT NOT NULL,
    requested_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    PRIMARY KEY (worker_id, environment)
) STRICT;

CREATE TABLE environment_tasks (
    id             BLOB NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    environment_id BLOB NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    text           TEXT NOT NULL CHECK (length(text) >= 1 AND length(text) <= 200),
    set_by         TEXT NOT NULL CHECK (set_by IN ('agent', 'user')),
    set_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    done_at        TEXT
) STRICT;
CREATE INDEX environment_tasks_by_environment ON environment_tasks (environment_id, set_at DESC);
CREATE UNIQUE INDEX environment_tasks_current ON environment_tasks (environment_id) WHERE done_at IS NULL;

CREATE TABLE agent_sessions (
    environment_id BLOB NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    id             TEXT NOT NULL,
    agent          TEXT NOT NULL,
    terminal       TEXT NOT NULL DEFAULT '',
    state          TEXT NOT NULL,
    step           TEXT NOT NULL DEFAULT '',
    message        TEXT NOT NULL DEFAULT '',
    since          TEXT NOT NULL,
    seen_at        TEXT,
    PRIMARY KEY (environment_id, id)
) STRICT;

CREATE TABLE editor_sessions (
    token_hash     BLOB    NOT NULL PRIMARY KEY,
    session_hash   BLOB    NOT NULL REFERENCES sessions (token_hash) ON DELETE CASCADE,
    environment_id BLOB    NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    ticket         INTEGER NOT NULL,
    expires_at     TEXT    NOT NULL
) STRICT;
CREATE INDEX editor_sessions_session ON editor_sessions (session_hash);

CREATE TABLE ssh_keys (
    id          BLOB NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    user_id     BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    public_key  TEXT NOT NULL,
    private_key BLOB NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'))
) STRICT;
CREATE INDEX ssh_keys_user ON ssh_keys (user_id);

CREATE TABLE ssh_login_keys (
    id          BLOB NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    user_id     BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    public_key  TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    UNIQUE (user_id, fingerprint)
) STRICT;

CREATE TABLE server_keys (
    name       TEXT    NOT NULL PRIMARY KEY,
    value      BLOB    NOT NULL,
    sealed     INTEGER NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'))
) STRICT;

CREATE TABLE packs (
    id          BLOB    NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    name        TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    owner_id    BLOB    REFERENCES users (id) ON DELETE CASCADE,
    team_id     BLOB    REFERENCES teams (id) ON DELETE CASCADE,
    builtin     INTEGER NOT NULL DEFAULT 0,
    attach      TEXT    NOT NULL DEFAULT 'listed'
        CHECK (attach IN ('listed', 'owner', 'team', 'everyone')),
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    squash      TEXT    NOT NULL DEFAULT 'all_squash'
        CHECK (squash IN ('all_squash', 'root_squash', 'no_root_squash')),
    same_name   BLOB,
    merged      TEXT    NOT NULL DEFAULT '[]',
    places      TEXT    NOT NULL DEFAULT '[]',
    away        INTEGER NOT NULL DEFAULT 0,
    CHECK (CASE WHEN builtin THEN owner_id IS NULL AND team_id IS NULL
                ELSE (owner_id IS NULL) <> (team_id IS NULL) END)
) STRICT;
CREATE UNIQUE INDEX packs_name ON packs
    (coalesce(owner_id, team_id, x'00000000000000000000000000000000'), lower(name));

CREATE TABLE pack_paths (
    pack_id    BLOB    NOT NULL REFERENCES packs (id) ON DELETE CASCADE,
    path       TEXT    NOT NULL,
    sensitive  INTEGER NOT NULL DEFAULT 0,
    send_every INTEGER,
    PRIMARY KEY (pack_id, path)
) STRICT;

CREATE TABLE copies (
    id         BLOB    NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    pack_id    BLOB    NOT NULL REFERENCES packs (id) ON DELETE CASCADE,
    owner_id   BLOB    REFERENCES users (id) ON DELETE CASCADE,
    team_id    BLOB    REFERENCES teams (id) ON DELETE CASCADE,
    personal   INTEGER NOT NULL DEFAULT 0,
    name       TEXT    NOT NULL DEFAULT '',
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    CHECK ((owner_id IS NULL) <> (team_id IS NULL)),
    CHECK (NOT personal OR owner_id IS NOT NULL)
) STRICT;
CREATE UNIQUE INDEX copies_personal ON copies (pack_id, owner_id) WHERE personal;

CREATE TABLE pack_defaults (
    user_id BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    pack_id BLOB NOT NULL REFERENCES packs (id) ON DELETE CASCADE,
    copy_id BLOB NOT NULL REFERENCES copies (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, pack_id)
) STRICT;

CREATE TABLE environment_copies (
    environment_id BLOB    NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    pack_id        BLOB    NOT NULL REFERENCES packs (id) ON DELETE CASCADE,
    copy_id        BLOB    NOT NULL REFERENCES copies (id) ON DELETE CASCADE,
    chosen         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (environment_id, pack_id)
) STRICT;

CREATE TABLE environment_routed (
    environment_id BLOB NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    copy_id        BLOB NOT NULL REFERENCES copies (id) ON DELETE CASCADE,
    PRIMARY KEY (environment_id, copy_id)
) STRICT;

CREATE TABLE environment_conflicts (
    environment_id BLOB NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    copy_id        BLOB NOT NULL REFERENCES copies (id) ON DELETE CASCADE,
    path           TEXT NOT NULL,
    found_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    resolution     TEXT CHECK (resolution IN ('shared', 'environment')),
    PRIMARY KEY (environment_id, copy_id, path)
) STRICT;

CREATE TABLE image_repositories (
    id          BLOB NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    path        TEXT NOT NULL UNIQUE,
    owner_id    BLOB REFERENCES users (id),
    team_id     BLOB REFERENCES teams (id),
    visibility  TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'shared')),
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    CHECK ((owner_id IS NULL) <> (team_id IS NULL))
) STRICT;
CREATE INDEX image_repositories_owner ON image_repositories (owner_id);
CREATE INDEX image_repositories_team ON image_repositories (team_id);

CREATE TABLE image_repository_collaborators (
    repository_id BLOB NOT NULL REFERENCES image_repositories (id) ON DELETE CASCADE,
    user_id       BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (repository_id, user_id)
) STRICT;

CREATE TABLE image_repository_team_collaborators (
    repository_id BLOB NOT NULL REFERENCES image_repositories (id) ON DELETE CASCADE,
    team_id       BLOB NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    PRIMARY KEY (repository_id, team_id)
) STRICT;

CREATE TABLE registry_blobs (
    digest     TEXT    NOT NULL PRIMARY KEY,
    size       INTEGER NOT NULL,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'))
) STRICT;

CREATE TABLE image_repository_blobs (
    repository_id BLOB NOT NULL REFERENCES image_repositories (id) ON DELETE CASCADE,
    digest        TEXT NOT NULL REFERENCES registry_blobs (digest),
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    PRIMARY KEY (repository_id, digest)
) STRICT;

CREATE TABLE image_manifests (
    repository_id BLOB NOT NULL REFERENCES image_repositories (id) ON DELETE CASCADE,
    digest        TEXT NOT NULL,
    media_type    TEXT NOT NULL,
    artifact_type TEXT NOT NULL DEFAULT '',
    subject       TEXT,
    annotations   TEXT,
    content       BLOB NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    PRIMARY KEY (repository_id, digest)
) STRICT;
CREATE INDEX image_manifests_subject ON image_manifests (repository_id, subject)
    WHERE subject IS NOT NULL;

CREATE TABLE image_manifest_refs (
    repository_id BLOB NOT NULL,
    digest        TEXT NOT NULL,
    ref           TEXT NOT NULL,
    PRIMARY KEY (repository_id, digest, ref),
    FOREIGN KEY (repository_id, digest)
        REFERENCES image_manifests (repository_id, digest) ON DELETE CASCADE
) STRICT;
CREATE INDEX image_manifest_refs_ref ON image_manifest_refs (repository_id, ref);

CREATE TABLE image_tags (
    repository_id BLOB NOT NULL,
    tag           TEXT NOT NULL,
    digest        TEXT NOT NULL,
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    PRIMARY KEY (repository_id, tag),
    FOREIGN KEY (repository_id, digest)
        REFERENCES image_manifests (repository_id, digest) ON DELETE CASCADE
) STRICT;

CREATE TABLE registry_uploads (
    id            BLOB NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    repository_id BLOB NOT NULL REFERENCES image_repositories (id) ON DELETE CASCADE,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'))
) STRICT;

CREATE TABLE dns_records (
    name       TEXT    NOT NULL,
    type       TEXT    NOT NULL CHECK (type = 'TXT'),
    value      TEXT    NOT NULL,
    ttl        INTEGER NOT NULL CHECK (ttl >= 0),
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    PRIMARY KEY (name, type, value)
) STRICT;

CREATE TABLE tls_storage (
    key      TEXT NOT NULL PRIMARY KEY,
    value    BLOB NOT NULL,
    modified TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now'))
) STRICT;

CREATE TABLE tls_locks (
    name       TEXT NOT NULL PRIMARY KEY,
    expires_at TEXT NOT NULL
) STRICT;

CREATE TABLE sign_in_grants (
    id          BLOB    NOT NULL PRIMARY KEY DEFAULT (randomblob(16)),
    device_hash BLOB    NOT NULL UNIQUE,
    user_code   TEXT    NOT NULL UNIQUE,
    name        TEXT    NOT NULL CHECK (length(name) >= 1 AND length(name) <= 100),
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    expires_at  TEXT    NOT NULL,
    approved_by BLOB    REFERENCES users (id) ON DELETE CASCADE,
    denied      INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX sign_in_grants_by_expiry ON sign_in_grants (expires_at);

CREATE TABLE link_persons (
    user_id   BLOB NOT NULL PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    person_id BLOB NOT NULL UNIQUE
) STRICT;

CREATE TABLE links (
    user_id     BLOB NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    place_id    BLOB NOT NULL,
    name        TEXT NOT NULL DEFAULT '',
    public_key  TEXT NOT NULL,
    url         TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now')),
    reached_at  TEXT,
    reach_error TEXT NOT NULL DEFAULT '',
    heard_at    TEXT,
    synced_at   TEXT,
    sync_error  TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (user_id, place_id)
) STRICT;

CREATE TABLE link_records (
    user_id   BLOB    NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind      TEXT    NOT NULL,
    record_id TEXT    NOT NULL,
    hlc       INTEGER NOT NULL,
    origin    BLOB    NOT NULL,
    digest    TEXT    NOT NULL DEFAULT '',
    deleted   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, kind, record_id)
) STRICT;

CREATE TABLE link_settings (
    user_id   BLOB    NOT NULL PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    bandwidth INTEGER NOT NULL DEFAULT 0
) STRICT;

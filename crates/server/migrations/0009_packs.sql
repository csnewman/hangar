-- The files environments share are packs and copies. A pack is a named
-- list of paths, owned by a person or a team, or built in; it holds no
-- files. A copy is one set of a pack's files: a person's own, or a shared
-- one a team (or a person) keeps. The profile is the built-in pack every
-- environment has, and each person's profile is their copy of it.
CREATE TABLE packs (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text NOT NULL,
    description text NOT NULL DEFAULT '',
    owner_id    uuid REFERENCES users (id) ON DELETE CASCADE,
    team_id     uuid REFERENCES teams (id) ON DELETE CASCADE,
    -- The server's: no person or team owns it, and admins change it.
    builtin     boolean NOT NULL DEFAULT false,
    CHECK (CASE WHEN builtin THEN owner_id IS NULL AND team_id IS NULL
        ELSE (owner_id IS NULL) <> (team_id IS NULL) END),
    -- Which environments it reaches without a template listing it: none
    -- (listed), its owner's, its team members', or everyone's.
    attach      text NOT NULL DEFAULT 'listed' CHECK (attach IN ('listed', 'owner', 'team', 'everyone')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX packs_name ON packs (coalesce(owner_id, team_id, '00000000-0000-0000-0000-000000000000'), lower(name));

-- A pack's paths: ~/ in the home directory, or absolute; a directory's
-- ending in a slash, one left out starting with "!". Every file under a
-- sensitive path is kept from environments given no sensitive files.
CREATE TABLE pack_paths (
    pack_id   uuid NOT NULL REFERENCES packs (id) ON DELETE CASCADE,
    path      text NOT NULL,
    sensitive boolean NOT NULL DEFAULT false,
    PRIMARY KEY (pack_id, path)
);

-- A copy's files are its directory on the files root, named by its id.
CREATE TABLE copies (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    pack_id    uuid NOT NULL REFERENCES packs (id) ON DELETE CASCADE,
    owner_id   uuid REFERENCES users (id) ON DELETE CASCADE,
    team_id    uuid REFERENCES teams (id) ON DELETE CASCADE,
    CHECK ((owner_id IS NULL) <> (team_id IS NULL)),
    -- A person's own copy: one each, made when they first need it.
    personal   boolean NOT NULL DEFAULT false,
    CHECK (NOT personal OR owner_id IS NOT NULL),
    name       text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX copies_personal ON copies (pack_id, owner_id) WHERE personal;

-- The copy a person uses of a pack unless an environment or its template
-- says otherwise.
CREATE TABLE pack_defaults (
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    pack_id uuid NOT NULL REFERENCES packs (id) ON DELETE CASCADE,
    copy_id uuid NOT NULL REFERENCES copies (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, pack_id)
);

-- The copy an environment uses of each pack it has had: chosen on its
-- page, or the one it was first given, kept so it does not change under
-- it.
CREATE TABLE environment_copies (
    environment_id uuid NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    pack_id        uuid NOT NULL REFERENCES packs (id) ON DELETE CASCADE,
    copy_id        uuid NOT NULL REFERENCES copies (id) ON DELETE CASCADE,
    chosen         boolean NOT NULL DEFAULT false,
    PRIMARY KEY (environment_id, pack_id)
);

-- The copies an environment's agent last said it routes to, which its
-- worker goes on serving it while it copies their files down.
CREATE TABLE environment_routed (
    environment_id uuid NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    copy_id        uuid NOT NULL REFERENCES copies (id) ON DELETE CASCADE,
    PRIMARY KEY (environment_id, copy_id)
);

-- A path an environment had a file of its own at, differing from its
-- copy's, when it became shared. The copy's is used; the environment's is
-- kept aside until someone chooses (resolution: shared or environment).
CREATE TABLE environment_conflicts (
    environment_id uuid NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    copy_id        uuid NOT NULL REFERENCES copies (id) ON DELETE CASCADE,
    path           text NOT NULL,
    found_at       timestamptz NOT NULL DEFAULT now(),
    resolution     text CHECK (resolution IN ('shared', 'environment')),
    PRIMARY KEY (environment_id, copy_id, path)
);

-- Moves the server makes on the files root once, at start: a copy whose
-- files are laid out relative to the home directory, moved under ~/, and
-- then paths moved from one copy to another.
CREATE TABLE copy_relayout (
    copy_id uuid PRIMARY KEY
);

CREATE TABLE copy_moves (
    from_copy uuid NOT NULL,
    to_copy   uuid NOT NULL,
    path      text NOT NULL,
    PRIMARY KEY (from_copy, to_copy, path)
);

-- The profile: what used to be the defaults in code, now the built-in
-- pack's paths, which admins change.
CREATE TEMPORARY TABLE mig_profile ON COMMIT DROP AS SELECT gen_random_uuid() AS id;

INSERT INTO packs (id, name, description, builtin, attach)
SELECT id, 'Profile', 'Claude''s settings and sign-in, git and VS Code settings, and CLI sign-ins, in every environment.',
    true, 'everyone' FROM mig_profile;

INSERT INTO pack_paths (pack_id, path, sensitive)
SELECT (SELECT id FROM mig_profile), p, s FROM (VALUES
    ('~/.claude/', false),
    ('~/.claude/.credentials.json', true),
    ('!~/.claude/projects/', false),
    ('!~/.claude/sessions/', false),
    ('!~/.claude/session-env/', false),
    ('!~/.claude/file-history/', false),
    ('!~/.claude/shell-snapshots/', false),
    ('!~/.claude/todos/', false),
    ('!~/.claude/tasks/', false),
    ('!~/.claude/jobs/', false),
    ('!~/.claude/ide/', false),
    ('!~/.claude/daemon/', false),
    ('!~/.claude/daemon.log', false),
    ('!~/.claude/statsig/', false),
    ('!~/.claude/telemetry/', false),
    ('!~/.claude/cache/', false),
    ('!~/.claude/paste-cache/', false),
    ('!~/.claude/downloads/', false),
    ('!~/.claude/backups/', false),
    ('!~/.claude/chrome/', false),
    ('!~/.claude/state/', false),
    ('!~/.claude/debug/', false),
    ('!~/.claude/logs/', false),
    ('!~/.claude/history.jsonl', false),
    ('!~/.claude/stats-cache.json', false),
    ('!~/.claude/settings.local.json', false),
    ('!~/.claude/.last-cleanup', false),
    ('!~/.claude/.last-update-result.json', false),
    ('~/.gitconfig', false),
    ('~/.git-credentials', true),
    ('~/.netrc', true),
    ('~/.config/gh/config.yml', false),
    ('~/.config/gh/hosts.yml', true),
    ('~/.config/glab-cli/config.yml', true),
    ('~/.docker/config.json', true),
    ('~/.npmrc', true),
    ('~/.pypirc', true),
    ('~/.aws/config', false),
    ('~/.aws/credentials', true),
    ('~/.kube/config', true),
    ('~/.config/gcloud/configurations/', false),
    ('~/.config/gcloud/active_config', false),
    ('~/.config/gcloud/application_default_credentials.json', true),
    ('~/.vscode-server-oss/data/User/settings.json', false),
    ('~/.vscode-server-oss/data/User/keybindings.json', false),
    ('~/.vscode-server-oss/data/User/snippets/', false)
) AS v (p, s);

-- Each person's profile is their copy of it, keeping its directory, whose
-- files move under ~/.
INSERT INTO copies (id, pack_id, owner_id, personal)
SELECT u.id, (SELECT id FROM mig_profile), u.id, true FROM users u;

INSERT INTO copy_relayout (copy_id) SELECT id FROM users;

-- The paths a person shared of their own are a pack of theirs, reaching
-- their environments, with a copy of its own their files move into.
CREATE TEMPORARY TABLE mig_mine ON COMMIT DROP AS
SELECT user_id, gen_random_uuid() AS pack_id, gen_random_uuid() AS copy_id
FROM (SELECT DISTINCT user_id FROM profile_paths) x;

INSERT INTO packs (id, name, owner_id, attach)
SELECT pack_id, 'My paths', user_id, 'owner' FROM mig_mine;

INSERT INTO pack_paths (pack_id, path)
SELECT m.pack_id, CASE WHEN p.path LIKE '!%' THEN '!~/' || substr(p.path, 2) ELSE '~/' || p.path END
FROM profile_paths p JOIN mig_mine m USING (user_id)
ON CONFLICT DO NOTHING;

INSERT INTO copies (id, pack_id, owner_id, personal)
SELECT copy_id, pack_id, user_id, true FROM mig_mine;

INSERT INTO copy_moves (from_copy, to_copy, path)
SELECT m.user_id, m.copy_id, '~/' || rtrim(p.path, '/')
FROM profile_paths p JOIN mig_mine m USING (user_id)
WHERE p.path NOT LIKE '!%'
ON CONFLICT DO NOTHING;

-- Each file pack is a pack. Its sets are its copies, keeping their
-- directories: a shared pack's one shared copy, owned as the pack is,
-- and a personal pack's one per person.
INSERT INTO packs (id, name, description, owner_id, team_id, attach, created_at, updated_at)
SELECT id, name, description, owner_id, team_id, 'listed', created_at, updated_at FROM file_packs;

INSERT INTO pack_paths (pack_id, path)
SELECT pack_id, path FROM file_pack_paths;

INSERT INTO copies (id, pack_id, owner_id, team_id, personal, name)
SELECT s.id, s.pack_id, CASE WHEN s.user_id IS NULL THEN k.owner_id ELSE s.user_id END,
    CASE WHEN s.user_id IS NULL THEN k.team_id END, s.user_id IS NOT NULL,
    CASE WHEN s.user_id IS NULL THEN 'Shared' ELSE '' END
FROM file_sets s JOIN file_packs k ON k.id = s.pack_id;

INSERT INTO copies (pack_id, owner_id, team_id, name)
SELECT k.id, k.owner_id, k.team_id, 'Shared' FROM file_packs k
WHERE NOT k.personal AND NOT EXISTS (SELECT 1 FROM file_sets s WHERE s.pack_id = k.id AND s.user_id IS NULL);

-- A file of a pack marked sensitive by hand is a sensitive path of the
-- pack; a pack's known credentials are sensitive, as they were by default.
INSERT INTO pack_paths (pack_id, path, sensitive)
SELECT s.pack_id, f.path, true FROM file_sensitivity f JOIN file_sets s ON s.id = f.set_id
WHERE s.pack_id IS NOT NULL AND f.sensitive
ON CONFLICT (pack_id, path) DO UPDATE SET sensitive = true;

UPDATE pack_paths SET sensitive = true
WHERE pack_id IN (SELECT id FROM file_packs) AND path IN (
    '~/.claude/.credentials.json', '~/.git-credentials', '~/.netrc', '~/.config/gh/hosts.yml',
    '~/.config/glab-cli/config.yml', '~/.docker/config.json', '~/.npmrc', '~/.pypirc', '~/.aws/credentials',
    '~/.kube/config', '~/.config/gcloud/application_default_credentials.json');

-- Specs list packs, a shared pack's pinned to its shared copy so everyone
-- goes on sharing one; and no_profile is no_self_attached.
CREATE TEMPORARY TABLE mig_shared ON COMMIT DROP AS
SELECT DISTINCT ON (pack_id) pack_id::text AS pack, id::text AS copy FROM copies
WHERE NOT personal AND pack_id IN (SELECT id FROM file_packs) ORDER BY pack_id, created_at;

UPDATE templates SET spec = (spec - 'file_packs') || jsonb_build_object('packs', coalesce((
    SELECT jsonb_agg(CASE WHEN m.copy IS NULL THEN jsonb_build_object('pack', x.p)
        ELSE jsonb_build_object('pack', x.p, 'copy', m.copy) END ORDER BY x.n)
    FROM jsonb_array_elements_text(spec->'file_packs') WITH ORDINALITY AS x (p, n)
    LEFT JOIN mig_shared m ON m.pack = x.p), '[]'))
WHERE jsonb_typeof(spec->'file_packs') = 'array';
UPDATE templates SET spec = spec - 'file_packs' WHERE spec ? 'file_packs';

UPDATE environments SET spec = (spec - 'file_packs') || jsonb_build_object('packs', coalesce((
    SELECT jsonb_agg(CASE WHEN m.copy IS NULL THEN jsonb_build_object('pack', x.p)
        ELSE jsonb_build_object('pack', x.p, 'copy', m.copy) END ORDER BY x.n)
    FROM jsonb_array_elements_text(spec->'file_packs') WITH ORDINALITY AS x (p, n)
    LEFT JOIN mig_shared m ON m.pack = x.p), '[]'))
WHERE jsonb_typeof(spec->'file_packs') = 'array';
UPDATE environments SET spec = spec - 'file_packs' WHERE spec ? 'file_packs';

UPDATE templates SET spec = (spec - 'no_profile') || '{"no_self_attached": true}' WHERE spec->'no_profile' = 'true';
UPDATE templates SET spec = spec - 'no_profile' WHERE spec ? 'no_profile';
UPDATE environments SET spec = (spec - 'no_profile') || '{"no_self_attached": true}' WHERE spec->'no_profile' = 'true';
UPDATE environments SET spec = spec - 'no_profile' WHERE spec ? 'no_profile';

-- What the packs replace. file_sets and set_files stay until every server
-- has copied what the blob store held (profile.MigrateFrom).
DROP TABLE file_sensitivity, file_pack_paths, profile_paths;
DROP TABLE file_packs CASCADE;

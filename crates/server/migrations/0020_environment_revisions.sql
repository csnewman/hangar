-- Each environment's revision, raised by every change to what a client is
-- shown of it, so a client can tell an older copy from a newer; and the
-- transaction that made that change, so a client resuming the event stream
-- from a snapshot is sent what the snapshot did not see.
CREATE SEQUENCE environment_revisions;
ALTER TABLE environments ADD COLUMN revision bigint NOT NULL DEFAULT nextval('environment_revisions');
ALTER TABLE environments ADD COLUMN changed_by xid8 NOT NULL DEFAULT pg_current_xact_id();

-- Usage alone is no change. Setting the revision to anything is one, which
-- is how a change to an environment's task or coding agents raises it.
CREATE FUNCTION revise_environment() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.revision IS NOT DISTINCT FROM OLD.revision
        AND (to_jsonb(OLD) - 'stats' - 'changed_by') = (to_jsonb(NEW) - 'stats' - 'changed_by') THEN
        RETURN NEW;
    END IF;
    NEW.revision := nextval('environment_revisions');
    NEW.changed_by := pg_current_xact_id();
    RETURN NEW;
END
$$;

CREATE TRIGGER environments_revised BEFORE UPDATE ON environments
    FOR EACH ROW EXECUTE FUNCTION revise_environment();

-- A change to an environment's task or agents raises its revision, whose
-- change notifies. Whoever writes them holds the environment's row first,
-- so this takes no lock out of order. An environment being deleted has no
-- row left to raise.
CREATE OR REPLACE FUNCTION notify_environment_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    env uuid;
BEGIN
    IF TG_TABLE_NAME = 'environments' THEN
        IF TG_OP = 'UPDATE' AND (to_jsonb(OLD) - 'stats') = (to_jsonb(NEW) - 'stats') THEN
            RETURN NULL;
        END IF;
        IF TG_OP = 'DELETE' THEN
            env := OLD.id;
        ELSE
            env := NEW.id;
        END IF;
        PERFORM pg_notify('environment_changed', env::text);
        RETURN NULL;
    END IF;
    IF TG_OP = 'DELETE' THEN
        env := OLD.environment_id;
    ELSE
        env := NEW.environment_id;
    END IF;
    UPDATE environments SET revision = -1 WHERE id = env;
    RETURN NULL;
END
$$;

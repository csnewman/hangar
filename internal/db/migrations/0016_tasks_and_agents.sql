-- What each environment is for: a line its coding agent sets as it works,
-- or its owner does. The one not done is its task; the rest are its
-- history.
CREATE TABLE environment_tasks (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    environment_id uuid NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    text           text NOT NULL CHECK (length(text) BETWEEN 1 AND 200),
    set_by         text NOT NULL CHECK (set_by IN ('agent', 'user')),
    set_at         timestamptz NOT NULL DEFAULT now(),
    done_at        timestamptz
);
CREATE INDEX environment_tasks_by_environment ON environment_tasks (environment_id, set_at DESC);
CREATE UNIQUE INDEX environment_tasks_current ON environment_tasks (environment_id) WHERE done_at IS NULL;

-- The coding agents' sessions in each running or suspended environment, as
-- its guest agent last reported them (api.AgentSession). They go when the
-- environment stops.
CREATE TABLE agent_sessions (
    environment_id uuid NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    id             text NOT NULL,
    agent          text NOT NULL,
    terminal       text NOT NULL DEFAULT '',
    state          text NOT NULL,
    step           text NOT NULL DEFAULT '',
    message        text NOT NULL DEFAULT '',
    since          timestamptz NOT NULL,
    -- When its owner last looked at it. A session that asked or finished
    -- before then does not need them.
    seen_at        timestamptz,
    PRIMARY KEY (environment_id, id)
);

-- environment_changed carries an environment's ID whenever something a
-- client shows of it changes, for the client API's event stream. Usage
-- figures, which its worker reports every few seconds, are left out.
CREATE FUNCTION notify_environment_changed() RETURNS trigger LANGUAGE plpgsql AS $$
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
    ELSIF TG_OP = 'DELETE' THEN
        env := OLD.environment_id;
    ELSE
        env := NEW.environment_id;
    END IF;
    PERFORM pg_notify('environment_changed', env::text);
    RETURN NULL;
END
$$;

CREATE TRIGGER environments_changed AFTER INSERT OR UPDATE OR DELETE ON environments
    FOR EACH ROW EXECUTE FUNCTION notify_environment_changed();
CREATE TRIGGER environment_tasks_changed AFTER INSERT OR UPDATE OR DELETE ON environment_tasks
    FOR EACH ROW EXECUTE FUNCTION notify_environment_changed();
CREATE TRIGGER agent_sessions_changed AFTER INSERT OR UPDATE OR DELETE ON agent_sessions
    FOR EACH ROW EXECUTE FUNCTION notify_environment_changed();

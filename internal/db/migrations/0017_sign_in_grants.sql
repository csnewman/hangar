-- An app signing in through the browser, in the shape of OAuth's device
-- authorization grant: the app asks for a grant and shows its code, the
-- person approves it on the server's own page, signed in as themselves,
-- and the app collects an access token made for them. The app holds the
-- device code, kept here only as a hash. A grant lasts ten minutes, and
-- goes once collected.
CREATE TABLE sign_in_grants (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    device_hash bytea NOT NULL UNIQUE,
    user_code   text NOT NULL UNIQUE,
    name        text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    approved_by uuid REFERENCES users (id) ON DELETE CASCADE,
    denied      boolean NOT NULL DEFAULT false
);
CREATE INDEX sign_in_grants_by_expiry ON sign_in_grants (expires_at);

-- Linked places (docs/places.md §4): a person's accounts in several
-- places, which share that person's own data. A person has one ID in every
-- place they link; a user with no row here is the person of their own ID.
CREATE TABLE link_persons (
    user_id   uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    person_id uuid NOT NULL UNIQUE
);

-- The places a person's account here is linked with: each one's ID, the
-- name the person knows it by, its public key (Ed25519, base64), the
-- address this place dials it at, when this place last reached it
-- (reached_at, or why not in reach_error), and when it last reached this
-- place (heard_at).
CREATE TABLE links (
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    place_id    uuid NOT NULL,
    name        text NOT NULL DEFAULT '',
    public_key  text NOT NULL,
    url         text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    reached_at  timestamptz,
    reach_error text NOT NULL DEFAULT '',
    heard_at    timestamptz,
    PRIMARY KEY (user_id, place_id)
);

-- What this place knows of each of a linked person's records (docs/places.md
-- §4.3): its kind and ID, the time of its last change on the places'
-- shared clock (milliseconds, kept ahead of every time seen) and the place
-- that made it, which together say which of two versions is newer, and the
-- SHA-256 of its content, by which a change made here is noticed. A
-- deleted record is kept as deleted, so the deletion travels too.
CREATE TABLE link_records (
    user_id   uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind      text NOT NULL,
    record_id text NOT NULL,
    hlc       bigint NOT NULL,
    origin    uuid NOT NULL,
    digest    text NOT NULL DEFAULT '',
    deleted   boolean NOT NULL DEFAULT false,
    PRIMARY KEY (user_id, kind, record_id)
);

-- When this place last exchanged records with a linked place, or why it
-- could not.
ALTER TABLE links ADD COLUMN synced_at timestamptz;
ALTER TABLE links ADD COLUMN sync_error text NOT NULL DEFAULT '';

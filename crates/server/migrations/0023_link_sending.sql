-- How often a file written in bursts is sent to linked places, per pack
-- path (docs/places.md §4.4): null at most once a second, 0 every change
-- as written, else at most once in so many seconds.
ALTER TABLE pack_paths ADD COLUMN send_every integer;

-- One of a person's own packs renamed, on linking, apart from another of
-- theirs of the same name made in another place (same_name), for the
-- person to merge or keep apart; and the packs merged into one, which
-- every linked place merges as it learns of them.
ALTER TABLE packs ADD COLUMN same_name uuid;
ALTER TABLE packs ADD COLUMN merged uuid[] NOT NULL DEFAULT '{}';

-- How fast this place sends a person's files to each place they link, in
-- bytes a second; 0 is no limit.
CREATE TABLE link_settings (
    user_id   uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    bandwidth bigint NOT NULL DEFAULT 0
);

-- The linked places a person's own pack is for (docs/places.md §4.4): none
-- listed is every one. A place left out holds the pack's record but not
-- the pack (away): it is hidden there, and its files go nowhere near it.
ALTER TABLE packs ADD COLUMN places uuid[] NOT NULL DEFAULT '{}';
ALTER TABLE packs ADD COLUMN away boolean NOT NULL DEFAULT false;

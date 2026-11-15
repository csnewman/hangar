-- The files root holds every copy's files, so the tables that kept them in
-- the blob store, and the one-off moves into the root, go.
DROP TABLE set_files, set_versions, set_locks, file_sets;
DROP TABLE copy_relayout, copy_moves, copy_chown;

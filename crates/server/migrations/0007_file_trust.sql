-- A file set's files are kept in a directory of their own on the files
-- root, which every worker serves its environments over NFS, rather than
-- here and in the blob store: what is left here is which files are kept
-- from environments not trusted with their owners' credentials, where
-- that differs from the path's default.
CREATE TABLE file_trust (
    set_id       uuid NOT NULL REFERENCES file_sets (id) ON DELETE CASCADE,
    path         text NOT NULL,
    trusted_only boolean NOT NULL,
    PRIMARY KEY (set_id, path)
);

INSERT INTO file_trust (set_id, path, trusted_only)
SELECT set_id, path, trusted_only FROM set_files WHERE NOT deleted;

-- Who an environment's callers act as on a pack's files, as an NFS
-- export's options say: all of them as the environments' user (one owner
-- for everything), root as nobody, or each as themselves.
ALTER TABLE packs ADD COLUMN squash text NOT NULL DEFAULT 'all_squash'
    CHECK (squash IN ('all_squash', 'root_squash', 'no_root_squash'));

-- The files root keeps real owners. What the control plane and the workers
-- made there before was made as root; it is the environments' user's, as
-- the server makes it once, at start.
CREATE TABLE copy_chown (
    copy_id uuid PRIMARY KEY
);

INSERT INTO copy_chown (copy_id) SELECT id FROM copies;

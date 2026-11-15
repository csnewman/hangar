-- What each worker reports of itself beyond its capacity: its build, and
-- what it can do beyond what its worker API level promises, which
-- placement matches against what an environment needs. Suspending is one
-- of them.
ALTER TABLE workers ADD COLUMN version text NOT NULL DEFAULT '';
ALTER TABLE workers ADD COLUMN features text[] NOT NULL DEFAULT '{}';
UPDATE workers SET features = ARRAY['suspend'] WHERE suspends;
ALTER TABLE workers DROP COLUMN suspends;

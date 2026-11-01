-- Whether each worker can suspend its environments, as it reports: all but
-- a Mac's, whose libkrun takes no snapshots.
ALTER TABLE workers ADD COLUMN suspends boolean NOT NULL DEFAULT false;
UPDATE workers SET suspends = (monitor = 'cloud-hypervisor');

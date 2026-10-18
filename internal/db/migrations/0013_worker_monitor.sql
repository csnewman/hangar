-- What runs each worker's environments, as it reports: cloud-hypervisor
-- on Linux, krun (libkrun) on macOS, which cannot suspend a machine or
-- resize its memory while it runs. Empty until a worker says.
ALTER TABLE workers ADD COLUMN monitor text NOT NULL DEFAULT '';

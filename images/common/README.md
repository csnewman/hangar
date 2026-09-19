# Shared image tree

`tree/` is overlaid onto every base via `ExtraTrees=` in each `mkosi.conf`.

It holds the parts of a Hangar environment that are the same on every
distribution: the docker storage driver and its drop-in, `/etc/fstab`, console
autologin, and the smoke test.

Networking is not here. Debian-family bases use systemd-networkd and RHEL-family
bases use NetworkManager, so each keeps its own configuration.

One copy, overlaid onto every base, so the bases cannot disagree about how an
environment is wired or about what the smoke test checks. A per-image copy of
any of this would only have to be kept in sync by hand.

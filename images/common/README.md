# Shared image trees

Every distribution builds two tiers, and each tier overlays its tree from
here via `ExtraTrees=` in its `mkosi.conf`, so the distributions cannot
disagree about how an environment is wired.

- `minimal/tree`: what every Hangar image has. The agent's unit, console
  autologin, the presets that decide which units start, and Docker's storage
  settings -- in `minimal` too, so a Docker the user installs uses the store
  the agent mounts at `/var/lib/docker`.
  `usr/share/hangar/image.json` says the image has no desktop. Units with
  nothing to do in an environment are masked (`etc/systemd/system/*` linked
  to `/dev/null`), since each costs boot time on few vCPUs:
  `systemd-modules-load` and `modprobe@` (the guest kernel has no modules),
  `systemd-firstboot` (mkosi sets what it would ask), and
  `systemd-network-generator` and `systemd-udev-load-credentials` (nothing
  passes them settings).
- `base/tree`: what `base` adds. The desktop -- `hangar-desktop.service`
  runs labwc headless, configured by `etc/xdg/labwc`, with waybar as its
  taskbar (`etc/xdg/waybar`) -- with `hangar-terminal` and `hangar-browser`, which its menu and shortcuts open
  under whatever name the distribution gives them, and the smoke test.
  `image.json` says the image has a desktop, which the worker checks before
  booting an environment whose template asks for one.

Networking is not here. Debian-family images use systemd-networkd and
RHEL-family images NetworkManager, so each distribution keeps its own.

`src/` holds programs compiled into `base` at build time: `hangar-glcheck`
and `hangar-vkcheck`, which report the renderer a guest actually got.

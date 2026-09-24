# Deploying Hangar

`compose.yaml` runs Hangar on real machines: the control plane (Postgres and
`hangar-server`) on one, and a worker on every machine that runs
environments. One machine can be both.

## The host

A worker machine needs, before its container starts:

- **Linux with KVM**: `/dev/kvm` present and usable (bare metal, or a VM with
  nested virtualisation). arm64 and x86_64.
- **Transparent huge pages for shared memory set to `advise`**. Guest memory is
  a shared memfd; without this, guests run three to five times slower, and the
  worker refuses to start. The setting belongs to the host kernel, so it is set
  there, not in the container:

      echo advise | sudo tee /sys/kernel/mm/transparent_hugepage/shmem_enabled
      echo 'w /sys/kernel/mm/transparent_hugepage/shmem_enabled - - - - advise' \
          | sudo tee /etc/tmpfiles.d/hangar-thp.conf    # across reboots

- **`/var/lib/hangar`** on a fast local filesystem with room for every
  environment's disks (each is sparse, up to `upper_gib + docker_gib`).
- Docker with Compose v2 and Buildx.

The control plane needs only Docker, and a DNS name that browsers reach: each
environment's editor is served on `e-<id>.<that name>`, so a wildcard record
(`*.hangar.example.com` as well as `hangar.example.com`) must point at it.

## Does the worker need `--privileged`?

No. It needs:

| | why |
|---|---|
| `/dev/kvm` | the virtual machines |
| `CAP_SYS_ADMIN` | passt (guest networking) and hangar-fs (the virtio-fs backend) sandbox themselves in new namespaces |
| `seccomp=unconfined`, `apparmor=unconfined` | Docker's default profiles refuse those namespace calls even with the capability |

and runs as root inside its container. `/dev/dri` is added for GPU
environments backed by a real host GPU. What it does not need is what
`--privileged` adds on top: every host device, writable `/sys` and `/proc/sys`,
and no capability bounding. The host setting above is why `/sys` stays
read-only.

Building base images (`--profile images`) needs the same three things, for
mkosi.

## Setting up

    cd deploy
    cp .env.example .env                  # then edit it
    openssl rand -hex 32 > bootstrap-token

Control plane:

    docker compose --profile control-plane up -d --build

Put TLS in front of port 8081 for `HANGAR_PUBLIC_URL` and its wildcard: the
`proxy` profile does it with Caddy and a certificate you supply in
`HANGAR_TLS_DIR` (a wildcard certificate needs a DNS challenge, which is why it
is not obtained automatically).

Each worker machine: copy `deploy/` and the same `bootstrap-token`, set
`server.url` and `node.name` in `worker.yaml`, then build a base image and
start the worker:

    IMAGE=ubuntu2604 docker compose --profile images run --rm --build build-image
    docker compose --profile worker up -d --build

The worker image builds everything a node supplies from source -- Cloud
Hypervisor with Hangar's patches, the virtio-fs and virtio-gpu backends, the
guest kernel, the guest agent and VS Code's server -- so its first build takes
a while and wants about 8 GiB of memory. Build it once and push it to a
registry for the rest of a fleet.

Finally, in Hangar, create a template for the image reference in
`worker.yaml` with the placement rule `runtime=cloud-hypervisor`.

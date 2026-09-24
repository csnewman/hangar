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

- **`/var/lib/hangar`** on a fast local Linux filesystem with room for every
  environment's disks (each is sparse, up to `upper_gib + docker_gib`) and
  the base images. An image is a root filesystem kept as a directory, served
  to its environments over virtio-fs, and must keep its owners: not a network
  or foreign filesystem that maps them.
- Docker with Compose v2 and Buildx.

The control plane needs Docker, a public address with ports 53 (UDP and
TCP), 443 and 80 open, and a DNS zone delegated to it (below).

## DNS and TLS

Each environment's editor is served on its own name, `e-<id>.<host>`, so
Hangar needs every name under its host and a certificate for all of them. A
wildcard certificate is only issued against ACME's DNS-01 challenge, which
means writing TXT records into the zone, so `hangar-server` is the zone's
authoritative DNS server: every name in it resolves to the control plane,
and the challenge's records are served from Postgres while they exist. It is
issued certificates from Let's Encrypt for `<host>` and `*.<host>`, keeps
them renewed, and serves HTTPS on 443 with them, redirecting 80 to it.

The zone is the host of `HANGAR_PUBLIC_URL`, say `hangar.example.com`. In the
parent zone, `example.com` at whoever serves it, delegate it with an NS
record and give the nameserver's address as glue:

    hangar.example.com.      NS    ns1.hangar.example.com.
    ns1.hangar.example.com.  A     203.0.113.10

where `203.0.113.10` is `HANGAR_DNS_ADDRESSES`. Registrars that manage the
parent's DNS call the second a glue record, a child nameserver, or a host
record; with an ordinary DNS provider it is just an A record. Add AAAA as
well if the control plane has IPv6. More than one nameserver name is fine,
all pointing at the control plane, listed in `HANGAR_DNS_NAMESERVERS`.

Check the delegation from outside once the control plane is up:

    dig +trace e-test.hangar.example.com
    dig @203.0.113.10 hangar.example.com NS

A new deployment is best tried against Let's Encrypt's staging CA
(`HANGAR_ACME_CA`), whose rate limits are generous, then switched to
production by emptying it. Certificates and the ACME account are kept in
Postgres.

On Ubuntu, systemd-resolved listens on `127.0.0.53:53`, which stops Docker
publishing port 53 on every address: set `HANGAR_PUBLISH_ADDRESS` to the
public address.

To use a TLS terminator and DNS of your own instead, set
`HANGAR_DNS_LISTEN`, `HANGAR_TLS_LISTEN` and `HANGAR_REDIRECT_LISTEN` to
empty and put it in front of `HANGAR_LISTEN`; it then needs a certificate
for both `<host>` and `*.<host>`.

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
    openssl rand -hex 32 > secret-key     # control plane only; back it up

Control plane:

    docker compose --profile control-plane up -d

Once the zone is delegated, the server is issued its certificates within a
minute or so; `docker compose logs server` shows it happening.

Each worker machine: copy `deploy/` and the same `bootstrap-token`, set
`server.url` and `node.name` in `worker.yaml`, then start the worker:

    docker compose --profile worker up -d

The server and worker images are pulled from `HANGAR_REGISTRY` at
`HANGAR_VERSION`: CI publishes both for arm64 and amd64, as `latest` and as
each commit on master. `up -d --build` builds them from this checkout
instead. The worker image holds everything a node supplies, built from
source -- Cloud Hypervisor with Hangar's patches, the virtio-fs and
virtio-gpu backends, the guest kernel, the guest agent and VS Code's server
-- so building it takes a while and wants about 8 GiB of memory.

Base images are pulled too, by the worker, the first time an environment
needs one: a template names an image reference, such as
`ghcr.io/csnewman/hangar/base-ubuntu2604:latest`, and the worker pulls its
own platform and unpacks it into `/var/lib/hangar/images`. It keeps that
copy until it is removed (Workers, then the worker's images), so a tag that
moves is pulled again only then. Private registries need credentials under
`vm.registries` in `worker.yaml`. To build a base image on the machine
instead, and have the worker use that for a reference:

    IMAGE=ubuntu2604 docker compose --profile images run --rm --build build-image

and name it under `vm.images` in `worker.yaml`.

Finally, in Hangar, create a template for an image with the placement rule
`runtime=cloud-hypervisor`.

# Deploying Hangar

`compose.yaml` runs Hangar on real machines: the control plane (Postgres,
`hangar-server` and RustFS, an S3-compatible object store) on one, and a
worker on every machine that runs environments. One machine can be both.

## The host

A worker machine needs, before its container starts:

- **Linux 6.8 or later, with KVM**: `/dev/kvm` present and usable (bare
  metal, or a VM with nested virtualisation). arm64 and x86_64. A pulled
  image is its layers stacked with overlayfs, one layer at a time, which
  overlayfs takes from 6.8.
- **Transparent huge pages for shared memory set to `advise`**. Guest memory is
  a shared memfd; without this, guests run three to five times slower, and the
  worker refuses to start. The setting belongs to the host kernel, so it is set
  there, not in the container:

      echo advise | sudo tee /sys/kernel/mm/transparent_hugepage/shmem_enabled
      echo 'w /sys/kernel/mm/transparent_hugepage/shmem_enabled - - - - advise' \
          | sudo tee /etc/tmpfiles.d/hangar-thp.conf    # across reboots

  A huge page needs a free 2 MB block, and a host whose free memory is
  fragmented, often by a large page cache, backs some guest memory with 4 KiB
  pages instead. The worker logs a warning when that happens. Keeping blocks
  free costs little memory, and matters most under nested virtualisation,
  where guest memory on 4 KiB pages is hundreds of times slower to touch:

      printf 'vm.compaction_proactiveness = 90\nvm.min_free_kbytes = 262144\n' \
          | sudo tee /etc/sysctl.d/60-hangar-thp.conf && sudo sysctl --system

- **`HANGAR_WORKER_DATA_DIR`** (`/var/lib/hangar` unless set) on a fast local Linux filesystem with room for every
  environment's disk (sparse, up to `upper_gib + docker_gib`) and
  the images. Each image layer is unpacked there once, however many images
  share it, and stacked into their root filesystems, from which each image
  gets an EROFS image its environments mount (`image_device` in
  `worker.yaml`); it must keep its owners: not a network or foreign
  filesystem that maps them.
- Docker with Compose v2 and Buildx.

The control plane needs Docker, a public address with ports 53 (UDP and
TCP), 443, 80 and 2222 open, and a DNS zone delegated to it (below).

## DNS and TLS

Each environment's editor is served on its own name, `code<short>.<host>`
(its six-character short ID), and
the registry on `registry.<host>`, so Hangar needs every name under its host and a certificate for all of them. A
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

    dig +trace test.hangar.example.com
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

To serve a certificate you already have instead of one from ACME, put it and
its key, as PEM, in `./tls` beside `compose.yaml`, which is mounted at
`/etc/hangar-server/tls`, and set

    HANGAR_TLS_CERT_FILE=/etc/hangar-server/tls/cert.pem
    HANGAR_TLS_KEY_FILE=/etc/hangar-server/tls/key.pem

The certificate file holds the chain, leaf first. Replacing the files is
picked up within a minute, without a restart. The server warns at start
about any name it serves that the certificate does not cover.

### Prefix host names

Some networks give a machine one name, `hangar1.example.com`, and resolve
every `<anything>-hangar1.example.com` to it too, under a wildcard
certificate for `*.example.com`. There Hangar names its other hosts with
the machine's name as their suffix instead of as their parent:

| | subdomain (default) | prefix |
|---|---|---|
| editors | `code<short>.hangar1.example.com` | `code<short>-hangar1.example.com` |
| registry | `registry.hangar1.example.com` | `registry-hangar1.example.com` |
| web servers | `<name>-env<short>.hangar1.example.com` | `<name>-env<short>-hangar1.example.com` |

Set `HANGAR_PUBLIC_URL=https://hangar1.example.com` and
`HANGAR_HOST_STYLE=prefix`, and give the wildcard certificate as above. The
network's own DNS answers for these names, so turn off Hangar's DNS server
with `HANGAR_DNS_LISTEN=` (empty); ACME is not used either, since it only
issues for the zone Hangar serves. Both kinds of name stay in the same site
as Hangar's own, which the editor, framed in Hangar's page, relies on.

## Environments' web servers

Whatever an environment serves is reached on `env<short>.<host>` (or
`env<short>-<host>` in the prefix style), where `<short>` is the
environment's six-character short ID: `envk3x9q2.hangar.example.com`. Any
name and a hyphen before it reaches the same, for a server that tells its
sites apart by host: `api-envk3x9q2.hangar.example.com`. HTTPS reaches the
environment's port 443, over TLS whatever certificate it has there, and
plain HTTP its port 80, with the host, path and headers as they were sent;
the browser is shown Hangar's certificate. Port 80 on the control plane
serves these names rather than redirecting them to HTTPS.

Each environment's page links to its plain name, and to any a template
names, `dashboard` and `customer-ui` say.

An environment is private until its owner makes it public, on its page:
private, a browser is sent to Hangar to sign in and back, and only people
who may use the environment get through; public, anyone who can reach
Hangar does. A DNS label is at most 63 characters, so the free name is at
most 53 (fewer in the prefix style, which shares the label with the
machine's name).

## The registry

The control plane is also a container registry, at `registry.<host>`,
covered by the same DNS and certificate as every other name under it:

    docker login registry.hangar.example.com -u <username>
    docker push registry.hangar.example.com/<username>/<image>:<tag>

The password is one of the user's access tokens (Account, then Access
tokens). Inside an environment nobody signs in: Docker there is set up to
ask Hangar, and is given a credential for the environment's owner that
lasts an hour, which is never stored and never has an administrator's
rights. An environment made from an untrusted template is given none. Each user pushes to their own namespace, their username in lower
case, and to the namespace of any team they are a member or admin of; a
repository is made private on its first push, and its owner shares it with
people, teams or everyone on the Images page. Name an image there in a
template and workers pull it through the control plane with their own
credential: they need no `vm.registries` entry for it, and nothing in
their network reaches the registry's name. A template may name only images
whoever saves it can pull.

Blobs are kept in the object store (below), and everything else about
them in Postgres, so every replica of the control plane shares them.
`HANGAR_REGISTRY_ENABLED=false` turns the registry off. Once an hour the control plane deletes what nothing needs: manifests
no tag, index or environment keeps, and blobs no repository uses, leaving
anything pushed within the hour. Deleting a tag or a repository frees its
space then.

## Your own images

An environment's image is a container image whose root filesystem is booted
as a machine, with systemd, so the easiest image of your own is Hangar's
base image with what your work needs added:

    # Dockerfile
    FROM ghcr.io/csnewman/hangar/base:ubuntu-26.04
    RUN apt-get update \
     && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
          postgresql-client python3-venv \
     && rm -rf /var/lib/apt/lists/*
    # Everything's user is dev, uid 1000, at /home/dev.
    COPY --chown=1000:1000 dotfiles/ /home/dev/
    # A service that starts with the environment.
    COPY my-agent.service /etc/systemd/system/
    RUN systemctl enable my-agent.service

For Rocky Linux the same, with `dnf`. The base image has Rocky's own
repositories; many everyday tools are in EPEL, which is added first:

    # Dockerfile
    FROM ghcr.io/csnewman/hangar/base:rocky-10
    RUN dnf install -y epel-release \
     && dnf install -y --setopt=install_weak_deps=False \
          postgresql python3-pip ripgrep \
     && dnf clean all
    COPY --chown=1000:1000 dotfiles/ /home/dev/
    COPY my-agent.service /etc/systemd/system/
    RUN systemctl enable my-agent.service

`minimal:<distro>` in place of `base:<distro>` starts from an image with no
desktop, Docker or everyday tools, for a headless environment that wants
little.

Build it for the workers' architecture, sign in to Hangar's registry with
an access token, and push:

    docker buildx build --platform linux/amd64,linux/arm64 \
        -t registry.hangar.example.com/<username>/devtools:1 --push .
    docker login registry.hangar.example.com -u <username>

then name `registry.hangar.example.com/<username>/devtools:1` as the image
in a template. Any registry works the same way; a private one needs
`vm.registries` in `worker.yaml`.

- Only the file system counts. `ENV`, `USER`, `WORKDIR`, `CMD`,
  `ENTRYPOINT`, `EXPOSE` and `VOLUME` are Docker's to run a container, and a
  machine boots its own init instead: put environment variables in
  `/etc/environment` or `/etc/profile.d/`, and what should run in a systemd
  unit.
- Workers keep each layer once, so an image on top of the base costs them
  only its own layers, and pulls only those.
- Pushing a new build to the same tag reaches new environments by itself.
  One made from the old build keeps it, and its page offers the upgrade
  (see Updating). Tag builds, or pin the base by digest, to know what an
  environment has.
- The base images' tags move with Hangar; rebuild on top of them to pick up
  its changes to the guest.

## SSH

The control plane is also an SSH gateway, on port 2222, in front of every
environment:

    ssh <environment>@<host> -p 2222

It signs people in with the public keys they add under Profile, then
Sign-in keys, and reaches the environment over the worker's own connection:
no environment is reachable from the network, and the workers need no
ports open for it. A person reaches their own environments by name, and
anyone else's they can see as `<owner>/<environment>`. An environment's
six-character short ID, on its page, names it whoever owns it
(`ssh k3x9q2@<host> -p 2222`), which is what the desktop app and the VS
Code extension use. Shells, commands,
port forwarding, `scp` and `sftp` all work, and so does VS Code's
Remote-SSH. The gateway's host key is made on first start and kept in
Postgres, sealed with the secret key, so every replica presents the same
one.

`HANGAR_SSH_LISTEN` moves it, or turns it off when empty.
`HANGAR_SSH_ADDRESS` is the `host:port` the UI shows people, when that is not
the public URL's host at the listen port -- behind a load balancer on port
22, say.

## Profiles

Each person's profile -- Claude's settings and sign-in, `.gitconfig`,
VS Code's settings, CLI sign-ins and the like -- is kept by the control
plane and synced into every environment they own. The Profile page lists
what is shared, and each person can add paths of their own there.
`HANGAR_PROFILE_PATHS` adds paths for everyone, beside the built-in ones:

    HANGAR_PROFILE_PATHS=.config/nvim/,.bash_aliases

Each is relative to the home directory, and a directory, ending in a slash,
shares everything under it. A path no one may share -- caches, and what
programs rewrite per machine, such as `.claude.json` -- stops the server
starting, and says why.

A profile's files are kept in the object store, and their paths and
versions, and which version of each object holds a file, in Postgres.

## The object store

Profiles' files and the registry's blobs are kept in an S3-compatible
object store: the `blobs` service, RustFS, with its data in
`$HANGAR_DATA_DIR/blobs`. The server signs in to it as `hangar` with a key
derived from `secret-key`, which the control plane's `data-dirs` step
derives for RustFS too, so there is nothing else to make or keep. To use a
store of your own, such as S3 or a RustFS cluster, set `HANGAR_BLOB_URL` to
`http(s)://<access key>@host[:port]/<bucket>` and `HANGAR_BLOB_SECRET_KEY`
to its secret key; `HANGAR_BLOB_ENCRYPT=true` asks it to encrypt what it
keeps (SSE-S3). Every replica of the control plane uses the same store.

What is kept is not encrypted by Hangar: profiles' files are in every
environment's disk as they are, so a copy encrypted here would protect
little. Encrypt the host's disks if that matters.

The server uses two buckets, making them if they are not there, and sets
their rules itself on every start:

| bucket | holds | rules |
|---|---|---|
| `<bucket>` | the registry's blobs, by digest | uploads' leftovers deleted after two days |
| `<bucket>-profiles` (`HANGAR_BLOB_PROFILE_BUCKET`) | profiles' files, as `<user>/<path>` | versioned; a version replaced is deleted after a day |

Each write of a profile file is a new version, and the server deletes the
one it replaced once the write is in the database; the versioning rule is
for the rare one it could not. So the store needs object versioning and
lifecycle rules, which S3, MinIO and RustFS have.

## Where the data is

Everything that lasts is in directories on the host, not Docker volumes,
so backups and the host's own tools see it and nothing in Docker can lose
it:

| | host directory |
|---|---|
| control plane's database | `$HANGAR_DATA_DIR/postgres` |
| profiles' files and the registry's blobs | `$HANGAR_DATA_DIR/blobs` (RustFS) |
| a worker's environments, images and caches | `$HANGAR_WORKER_DATA_DIR` |
| secrets | `bootstrap-token` and `secret-key` beside `compose.yaml` |

`HANGAR_DATA_DIR` is `/var/lib/hangar-server` and `HANGAR_WORKER_DATA_DIR`
`/var/lib/hangar` unless `.env` says otherwise; the worker sees its own as
`/var/lib/hangar` either way, which is what `worker.yaml` names. To keep
everything in one directory, set `HANGAR_WORKER_DATA_DIR` to
`$HANGAR_DATA_DIR/worker` (spelled out: `.env` does not expand it); a
worker already in use is moved with it stopped:

    docker compose --profile worker down
    sudo mv /var/lib/hangar /var/lib/hangar-server/worker
    docker compose --profile worker up -d
 Back up the database, the object store's directory and the secrets
together: the database without `secret-key` loses users' SSH keys. Stop the control plane first, or dump
the database live:

    docker compose exec -T postgres pg_dump -U hangar hangar > hangar.sql

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

## Updating

On each machine, from `deploy/`, pull the new images and recreate what
changed, naming the profiles that machine runs:

    docker compose --profile control-plane --profile worker pull
    docker compose --profile control-plane --profile worker up -d

`latest` moves with every commit on master; set `HANGAR_VERSION` to a
commit to stay on one, and change it to update. Copy the new `compose.yaml`
across too when it has changed. Nothing else is needed:

- The server brings the database up to date as it starts, and workers
  reconnect to it on their own.
- A worker that is replaced powers its environments off first; their disks
  are kept, and those that were running start again once it is back,
  booting the new worker's kernel and agent. Anything unsaved in them is
  lost, so update when people can stop.
- A suspended environment stays suspended and resumes on the new worker. A
  release that changes the machine underneath, such as Cloud Hypervisor,
  may not resume one suspended on the old; stop environments rather than
  suspend them before such an update.
- An environment's image stays the copy it was made over; the page offers
  a newer build of it when there is one.

Images are pulled too, by the worker, the first time an environment needs
one: a template names an image reference, such as
`ghcr.io/csnewman/hangar/base:ubuntu-26.04`, and the worker pulls its own
platform and unpacks it into `/var/lib/hangar/images`. Each distribution
(`ubuntu-26.04`, `rocky-10`) comes in two tiers: `minimal`, which boots,
clones repositories and has the package manager to add the rest, and
`base`, which adds Docker, the GPU's drivers, a desktop, a browser and the
everyday tools. `base` is `minimal` plus one layer, so a worker that has
one fetches only the difference for the other. `:ubuntu` and `:rocky`
follow the newest release of each.

The worker looks the tag up each time it makes an environment, and pulls
the image again when the tag has moved. An environment stays on the copy it
was made from, since its writable layer was made over that one, and the
environment's page shows which copy that is. A copy nothing uses once its
tag has moved on is deleted; Workers, then the worker's images, lists the
copies a worker holds. Private registries need credentials under
`vm.registries` in `worker.yaml`. To build a distribution's images on the
machine instead, and have the worker use them for a reference:

    IMAGE=ubuntu-26.04 docker compose --profile images run --rm --build build-image

and name them under `vm.images` in `worker.yaml`. Each build carries an ID,
which the worker compares as it would a registry's digest, so rebuilding
reaches new environments without touching the worker.

Finally, in Hangar, create a template for an image with the placement rule
`runtime=cloud-hypervisor`.

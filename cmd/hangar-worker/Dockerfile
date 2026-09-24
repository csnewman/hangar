# hangar-worker with everything a node supplies to its environments, built
# from source. Build from the repository root, on the architecture the node
# runs (guests are the host's architecture):
#
#   docker build -f cmd/hangar-worker/Dockerfile -t hangar-worker .
#   docker build -f cmd/hangar-worker/Dockerfile --target imagebuilder -t hangar-imagebuilder .
#
# The worker image holds:
#
#   /usr/local/bin/hangar-worker      the worker
#   /usr/local/bin/cloud-hypervisor   upstream at CH_COMMIT plus internal/ch/upstream
#   /usr/local/bin/hangar-fs          virtio-fs backend (fs/)
#   /usr/local/bin/hangar-gpu         virtio-gpu backend (gpu/), with the patched
#                                     virglrenderer in /usr/local/lib
#   /usr/share/hangar/vmlinuz         the guest kernel (internal/kernel)
#   /usr/share/hangar/hangar-agent    the guest agent, each boot's initramfs
#                                     (as init) and then the agent
#   /usr/share/hangar/editor.ext4     VS Code's server (editor/), attached to
#                                     every environment
#
# Base images are not in it: an image is a root filesystem, built by the
# imagebuilder target (see deploy/compose.yaml), which needs mount and
# namespace permissions that `docker build` does not have.
#
# The editor stage builds VS Code and wants 8 GiB of memory and some time.

ARG UBUNTU=ubuntu:26.04

# ---------------------------------------------------------------- Go
FROM golang:1.27 AS go
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY cmd/ cmd/
COPY internal/ internal/
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/hangar-worker ./cmd/hangar-worker \
 && CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/hangar-agent ./cmd/hangar-agent

# ---------------------------------------------------------------- guest kernel
FROM ${UBUNTU} AS kernel
ARG KERNEL_VERSION=6.18.52
COPY internal/kernel/scripts/build.sh /scripts/build.sh
COPY internal/kernel/hangar.config internal/kernel/hangar-arm64.config internal/kernel/hangar-x86_64.config /cfg/
COPY internal/kernel/patches/ /cfg/patches/
RUN case "$(uname -m)" in aarch64) KARCH=arm64 ;; *) KARCH=x86_64 ;; esac \
 && mkdir -p /out \
 && KERNEL_VERSION=$KERNEL_VERSION KARCH=$KARCH sh /scripts/build.sh

# ---------------------------------------------------------------- Rust toolchain
FROM ${UBUNTU} AS rust
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      build-essential ca-certificates curl git pkg-config \
      meson ninja-build python3-yaml python3-mako clang libclang-dev \
      libepoxy-dev libdrm-dev libgbm-dev libvulkan-dev libegl-dev >/dev/null \
 && curl -fsSL https://sh.rustup.rs | sh -s -- -y -q --profile minimal
ENV PATH=/root/.cargo/bin:$PATH

# ---------------------------------------------------------------- Cloud Hypervisor
FROM rust AS cloud-hypervisor
# Upstream main at the commit the patches in internal/ch/upstream are carried
# against.
ARG CH_COMMIT=1d63a241b3c78d103b12956e3a2b9dd5d7a5df2e
COPY internal/ch/upstream/*.patch /patches/
RUN git init -q /src && cd /src \
 && git fetch -q --depth 1 https://github.com/cloud-hypervisor/cloud-hypervisor.git "$CH_COMMIT" \
 && git checkout -q FETCH_HEAD \
 && git -c user.name=build -c user.email=build@localhost am -q /patches/*.patch \
 && cargo build --release --locked \
 && install -D target/release/cloud-hypervisor /out/cloud-hypervisor

# ---------------------------------------------------------------- hangar-fs
FROM rust AS fs
COPY fs/Cargo.toml fs/Cargo.lock /src/
COPY fs/src/ /src/src/
RUN cd /src && cargo build --release --locked \
 && install -D target/release/hangar-fs /out/hangar-fs

# ---------------------------------------------------------------- hangar-gpu
FROM rust AS gpu
# virglrenderer is built with /usr/local as its prefix, which is where the
# runtime stage puts it: the Venus render server's path is compiled in.
COPY gpu/virglrenderer/ /src/virglrenderer/
RUN sh /src/virglrenderer/build.sh /usr/local
COPY gpu/Cargo.toml gpu/Cargo.lock /src/
COPY gpu/src/ /src/src/
RUN cd /src \
 && PKG_CONFIG_PATH=/usr/local/lib/$(gcc -dumpmachine)/pkgconfig cargo build --release --locked \
 && install -D target/release/hangar-gpu /out/hangar-gpu

# ---------------------------------------------------------------- editor
FROM ${UBUNTU} AS editor
ARG NODE_VERSION=24.18.0
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      build-essential ca-certificates curl git python3 pkg-config xz-utils e2fsprogs \
      libx11-dev libxkbfile-dev libsecret-1-dev libkrb5-dev >/dev/null \
 && case "$(uname -m)" in aarch64) a=arm64 ;; *) a=x64 ;; esac \
 && curl -fsSL "https://nodejs.org/dist/v$NODE_VERSION/node-v$NODE_VERSION-linux-$a.tar.xz" \
    | tar -xJ -C /usr/local --strip-components=1
COPY editor/ /src/editor/
RUN HANGAR_EDITOR_WORK=/work /src/editor/build.sh /out \
 && mv /out/editor-*.ext4 /out/editor.ext4

# ---------------------------------------------------------------- worker
FROM ${UBUNTU} AS worker
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      ca-certificates passt e2fsprogs coreutils \
      libepoxy0 libgbm1 libegl1 libgles2 libdrm2 libvulkan1 \
      libgl1-mesa-dri mesa-vulkan-drivers >/dev/null \
 && rm -rf /var/lib/apt/lists/*
COPY --from=gpu /usr/local/lib/ /usr/local/lib/
COPY --from=gpu /usr/local/libexec/ /usr/local/libexec/
RUN ldconfig
COPY --from=cloud-hypervisor /out/cloud-hypervisor /usr/local/bin/
COPY --from=fs /out/hangar-fs /usr/local/bin/
COPY --from=gpu /out/hangar-gpu /usr/local/bin/
COPY --from=go /out/hangar-worker /usr/local/bin/
COPY --from=go /out/hangar-agent /usr/share/hangar/hangar-agent
COPY --from=kernel /out/vmlinuz /usr/share/hangar/vmlinuz
COPY --from=editor /out/editor.ext4 /usr/share/hangar/editor.ext4
ENTRYPOINT ["hangar-worker"]
CMD ["-config", "/etc/hangar/worker.yaml"]

# ---------------------------------------------------------------- imagebuilder
# Builds a base image the way `hangar build` does: /out/rootfs, the image's
# root filesystem, which a worker serves to its environments over virtio-fs.
# Run it with CAP_SYS_ADMIN and seccomp and AppArmor unconfined,
# IMAGE=<images/ subdir>, and /out mounted from a Linux filesystem: the tree
# keeps the image's owners.
FROM ${UBUNTU} AS imagebuilder
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      mkosi ubuntu-keyring dpkg-dev apt-utils uidmap dnf rpm ca-certificates \
      zstd gzip python3-minimal >/dev/null
COPY internal/image/scripts/ /scripts/
COPY images/ /cfg/
ENV IMAGE=ubuntu2604
CMD ["sh", "/scripts/mkosi.sh"]

FROM worker

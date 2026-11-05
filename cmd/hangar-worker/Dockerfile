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
#   /usr/local/bin/hangar-gpu         virtio-gpu backend (gpu/), with the patched
#                                     virglrenderer in /usr/local/lib
#   /usr/local/bin/hangar-krun        runs a machine under libkrun (runtime: krun),
#                                     with libkrun in /usr/local/lib: the fork's
#                                     hangar branch at krunHangar in dev/krun-build.sh
#   /usr/share/hangar/vmlinuz         the guest kernel (internal/kernel)
#   /usr/share/hangar/vmlinux         on amd64, the same kernel as ELF, which
#                                     libkrun boots there
#   /usr/share/hangar/hangar-agent    the guest agent, each boot's initramfs
#                                     (as init) and then the agent
#   /usr/share/hangar/editor.ext4     VS Code's server (editor/), attached to
#                                     every environment
#
# Environment images are not in it: an image is a root filesystem, built by the
# imagebuilder target (see deploy/compose.yaml), which needs mount and
# namespace permissions that `docker build` does not have.
#
# The editor stage builds VS Code and wants 8 GiB of memory and some time.
#
# Every stage that builds removes what it built from in the same step, and
# keeps only /out (or what it installed): the build cache stores each
# stage's layers, and a source tree or a target directory left in one is
# gigabytes of it.

ARG UBUNTU=ubuntu:26.04

# ---------------------------------------------------------------- Go
FROM golang:1.27 AS go
# VERSION names the build.
ARG VERSION=dev
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY cmd/ cmd/
COPY internal/ internal/
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w -X github.com/csnewman/hangar/internal/version.Version=$VERSION" -o /out/hangar-worker ./cmd/hangar-worker \
 && CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/hangar-agent ./cmd/hangar-agent \
 && rm -rf "$(go env GOCACHE)"

# ---------------------------------------------------------------- guest kernel
FROM ${UBUNTU} AS kernel
# KERNEL_VERSION overrides internal/kernel/version, the version `hangar kernel`
# builds too.
ARG KERNEL_VERSION
COPY internal/kernel/scripts/build.sh /scripts/build.sh
COPY internal/kernel/version /cfg/version
COPY internal/kernel/hangar.config internal/kernel/hangar-arm64.config internal/kernel/hangar-x86_64.config /cfg/
COPY internal/kernel/patches/ /cfg/patches/
COPY internal/kernel/tree/ /cfg/tree/
RUN case "$(uname -m)" in aarch64) KARCH=arm64 ;; *) KARCH=x86_64 ;; esac \
 && mkdir -p /out \
 && KERNEL_VERSION=$KERNEL_VERSION KARCH=$KARCH sh /scripts/build.sh \
 && rm -rf /src /var/lib/apt/lists/*

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
 && install -D target/release/cloud-hypervisor /out/cloud-hypervisor \
 && cd / && rm -rf /src /root/.cargo/registry /root/.cargo/git

# ---------------------------------------------------------------- virglrenderer
FROM rust AS virgl
# virglrenderer is built with /usr/local as its prefix, which is where the
# runtime stage puts it: the Venus render server's path is compiled in.
COPY gpu/virglrenderer/ /src/virglrenderer/
RUN sh /src/virglrenderer/build.sh /usr/local

# ---------------------------------------------------------------- hangar-gpu
FROM virgl AS gpu
COPY gpu/Cargo.toml gpu/Cargo.lock /src/
COPY gpu/src/ /src/src/
RUN cd /src \
 && PKG_CONFIG_PATH=/usr/local/lib/$(gcc -dumpmachine)/pkgconfig cargo build --release --locked \
 && install -D target/release/hangar-gpu /out/hangar-gpu \
 && rm -rf /src/target /root/.cargo/registry /root/.cargo/git

# ---------------------------------------------------------------- libkrun
# libkrun is pinned where the dev hosts build it from: krunRepo and
# krunHangar in dev/krun-build.sh. Only those two lines go on to the build,
# so it runs again when the pin moves and not whenever the script changes.
FROM ${UBUNTU} AS krun-pin
COPY dev/krun-build.sh /krun-build.sh
RUN grep -E '^krun(Repo|Hangar)=' /krun-build.sh > /pin \
 && grep -qE '^krunHangar=[0-9a-f]{40}$' /pin

FROM virgl AS krun
COPY --from=krun-pin /pin /pin
# Built without its guest init (Hangar's agent is init), though the build
# still makes one, linked against musl. Its C bindings are generated and run
# through rustfmt.
RUN . /pin \
 && rustup target add "$(uname -m)-unknown-linux-musl" \
 && rustup component add rustfmt \
 && git init -q /src && cd /src \
 && git fetch -q --depth 1 "$krunRepo" "$krunHangar" \
 && git checkout -q FETCH_HEAD \
 && PKG_CONFIG_PATH=/usr/local/lib/$(gcc -dumpmachine)/pkgconfig make -s INIT_BLOB=0 BLK=1 NET=1 GPU=1 \
 && install -D target/release/libkrun.so.2.0.0 /out/libkrun.so.2 \
 && ln -s libkrun.so.2 /out/libkrun.so \
 && cp -rL include /out/include \
 && cd / && rm -rf /src /root/.cargo/registry /root/.cargo/git
# hangar-krun links libkrun with cgo, against this stage's C libraries,
# which are the worker's.
COPY --from=golang:1.27 /usr/local/go /usr/local/go
WORKDIR /hk
COPY go.mod go.sum ./
RUN /usr/local/go/bin/go mod download
COPY cmd/ cmd/
COPY internal/ internal/
RUN CGO_CFLAGS=-I/out/include CGO_LDFLAGS=-L/out \
    /usr/local/go/bin/go build -tags krun -trimpath -ldflags="-s -w" -o /out/hangar-krun ./cmd/hangar-krun \
 && rm -rf "$(/usr/local/go/bin/go env GOCACHE)" "$(/usr/local/go/bin/go env GOMODCACHE)"

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
 && mv /out/editor-*.ext4 /out/editor.ext4 \
 && find /out -mindepth 1 -maxdepth 1 ! -name editor.ext4 -exec rm -rf {} + \
 && rm -rf /work /root/.npm /root/.cache

# ---------------------------------------------------------------- worker
FROM ${UBUNTU} AS worker
# libopengl0 is desktop OpenGL through EGL, which libepoxy loads for
# virglrenderer and aborts without: the GPU backend would die as the guest
# first asked what it offers.
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      ca-certificates passt e2fsprogs coreutils \
      libepoxy0 libgbm1 libegl1 libgles2 libopengl0 libdrm2 libvulkan1 \
      libgl1-mesa-dri mesa-vulkan-drivers >/dev/null \
 && rm -rf /var/lib/apt/lists/*
COPY --from=gpu /usr/local/lib/ /usr/local/lib/
COPY --from=gpu /usr/local/libexec/ /usr/local/libexec/
COPY --from=krun /out/libkrun.so.2 /usr/local/lib/
RUN ldconfig
COPY --from=cloud-hypervisor /out/cloud-hypervisor /usr/local/bin/
COPY --from=gpu /out/hangar-gpu /usr/local/bin/
COPY --from=krun /out/hangar-krun /usr/local/bin/
COPY --from=go /out/hangar-worker /usr/local/bin/
COPY --from=go /out/hangar-agent /usr/share/hangar/hangar-agent
# vmlinux is amd64's alone; the pattern lets the copy go ahead without it.
COPY --from=kernel /out/vmlinuz /out/vmlinu[x] /usr/share/hangar/
COPY --from=editor /out/editor.ext4 /usr/share/hangar/editor.ext4
ENTRYPOINT ["hangar-worker"]
CMD ["-config", "/etc/hangar/worker.yaml"]

# ---------------------------------------------------------------- imagebuilder
# Builds a distribution's images the way `hangar build` does: /out/minimal
# and /out/base, each holding rootfs, the image's root filesystem, which a
# worker boots its environments from. Run it with
# CAP_SYS_ADMIN and seccomp and AppArmor unconfined, IMAGE=<images/ subdir>,
# and /out mounted from a Linux filesystem: the trees keep the images' owners.
FROM ${UBUNTU} AS imagebuilder
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      mkosi ubuntu-keyring dpkg-dev apt-utils uidmap dnf rpm ca-certificates \
      zstd gzip python3-minimal >/dev/null
COPY internal/image/scripts/ /scripts/
COPY images/ /cfg/
ENV IMAGE=ubuntu-26.04
CMD ["sh", "/scripts/mkosi.sh"]

FROM worker

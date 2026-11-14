# hangar-worker with everything a node supplies to its environments, built
# from source. Build from the repository root, on the architecture the node
# runs (guests are the host's architecture):
#
#   docker build -f crates/worker/Dockerfile -t hangar-worker .
#   docker build -f crates/worker/Dockerfile --target imagebuilder -t hangar-imagebuilder .
#
# The worker image holds:
#
#   /usr/local/bin/hangar-worker      the worker (crates/worker)
#   /usr/local/bin/cloud-hypervisor   upstream at CH_COMMIT plus cloud-hypervisor/
#   /usr/local/bin/hangar-gpu         virtio-gpu backend (gpu/), with the patched
#                                     virglrenderer in /usr/local/lib
#   /usr/local/bin/hangar-krun        runs a machine under libkrun (runtime: krun;
#                                     crates/krun), with libkrun linked in: the
#                                     fork's hangar branch at krunHangar in
#                                     dev/krun-build.sh
#   /usr/share/hangar/vmlinuz         the guest kernel (internal/kernel)
#   /usr/share/hangar/vmlinux         on amd64, the same kernel as ELF, which
#                                     libkrun boots there
#   /usr/share/hangar/hangar-agent    the guest agent (crates/agent), static,
#                                     each boot's initramfs (as init) and
#                                     then the agent
#
# Environment images are not in it: an image is a root filesystem, built by the
# imagebuilder target (see deploy/compose.yaml), which needs mount and
# namespace permissions that `docker build` does not have.
#
# Every stage that builds removes what it built from in the same step, and
# keeps only /out (or what it installed): the build cache stores each
# stage's layers, and a source tree or a target directory left in one is
# gigabytes of it.

ARG UBUNTU=ubuntu:26.04

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

# ---------------------------------------------------------------- the worker and the guest agent
# The agent is static, against musl: the guest's own C library is whatever
# its image has. The worker is against the image's own.
FROM rust AS crates
WORKDIR /src
# The whole workspace: only two members are built, but cargo reads every
# member's manifest and finds each one's targets.
COPY crates/ ./
RUN target="$(uname -m)-unknown-linux-musl" \
 && rustup target add "$target" \
 && cargo build --release --locked --target "$target" -p hangar-agent \
 && install -D "target/$target/release/hangar-agent" /out/hangar-agent \
 && cargo build --release --locked -p hangar-worker \
 && install -D target/release/hangar-worker /out/hangar-worker \
 && cd / && rm -rf /src /root/.cargo/registry /root/.cargo/git

# ---------------------------------------------------------------- Cloud Hypervisor
FROM rust AS cloud-hypervisor
# Upstream main at the commit the patches in cloud-hypervisor/ are carried
# against.
ARG CH_COMMIT=1d63a241b3c78d103b12956e3a2b9dd5d7a5df2e
COPY cloud-hypervisor/*.patch /patches/
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
RUN . /pin \
 && git init -q /libkrun && cd /libkrun \
 && git fetch -q --depth 1 "$krunRepo" "$krunHangar" \
 && git checkout -q FETCH_HEAD
# hangar-krun links the checkout as a crate (crates/krun/libkrun, the link
# dev/krun-build.sh makes on a dev host), and virglrenderer from this
# stage's /usr/local, which is the worker's. hangar-api, a path dependency,
# takes its edition from the workspace, so the whole of crates/ comes in.
COPY crates/ /src/
RUN ln -s /libkrun /src/krun/libkrun && cd /src/krun \
 && PKG_CONFIG_PATH=/usr/local/lib/$(gcc -dumpmachine)/pkgconfig cargo build --release --locked --features gpu \
 && install -D target/release/hangar-krun /out/hangar-krun \
 && cd / && rm -rf /src /libkrun /root/.cargo/registry /root/.cargo/git

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
RUN ldconfig
COPY --from=cloud-hypervisor /out/cloud-hypervisor /usr/local/bin/
COPY --from=gpu /out/hangar-gpu /usr/local/bin/
COPY --from=krun /out/hangar-krun /usr/local/bin/
COPY --from=crates /out/hangar-worker /usr/local/bin/
COPY --from=crates /out/hangar-agent /usr/share/hangar/hangar-agent
# vmlinux is amd64's alone; the pattern lets the copy go ahead without it.
COPY --from=kernel /out/vmlinuz /out/vmlinu[x] /usr/share/hangar/
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

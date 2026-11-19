# hangar-server with its web UI built in. Build from the repository root:
#
#   docker build -f crates/server/Dockerfile -t hangar-server .
#
# Both builds run on the machine building the image: the UI is the same for
# every platform, and the server is static, against musl, cross-compiled for
# the platform asked for (clang compiles ring's and SQLite's C, rust-lld
# links against Rust's own musl), so no emulation is involved and the image
# that runs holds one binary. The C is compiled against musl's headers for
# the target (Debian's musl-dev, installed for that architecture), and kept
# from the build machine's own (-nostdlibinc), which are glibc's.

FROM --platform=$BUILDPLATFORM node:24 AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM --platform=$BUILDPLATFORM rust:1 AS build
ARG TARGETARCH
# VERSION names the build, as /api/v1/version reports it.
ARG VERSION=dev
RUN { [ "$TARGETARCH" = "$(dpkg --print-architecture)" ] || dpkg --add-architecture "$TARGETARCH"; } \
 && apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      clang llvm "musl-dev:$TARGETARCH" >/dev/null
WORKDIR /src
COPY api/ api/
COPY crates/ crates/
COPY --from=web /src/web/dist web/dist
RUN case "$TARGETARCH" in arm64) arch=aarch64 ;; amd64) arch=x86_64 ;; *) echo "no Rust target for $TARGETARCH" >&2; exit 1 ;; esac \
 && target=$arch-unknown-linux-musl \
 && rustup target add "$target" \
 && cd crates \
 && env "CARGO_TARGET_$(echo "$target" | tr a-z- A-Z_)_LINKER=rust-lld" "AR_${arch}_unknown_linux_musl=llvm-ar" \
      "CFLAGS_${arch}_unknown_linux_musl=-nostdlibinc -isystem /usr/include/${arch}-linux-musl" \
      HANGAR_VERSION="$VERSION" cargo build --release --locked --target "$target" -p hangar-server \
 && install -D "target/$target/release/hangar-server" /out/hangar-server \
 && rm -rf target /usr/local/cargo/registry

FROM gcr.io/distroless/static-debian13:nonroot
COPY --from=build /out/hangar-server /hangar-server
EXPOSE 8081
ENTRYPOINT ["/hangar-server"]

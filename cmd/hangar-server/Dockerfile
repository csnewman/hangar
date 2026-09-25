# hangar-server with its web UI built in. Build from the repository root:
#
#   docker build -f cmd/hangar-server/Dockerfile -t hangar-server .
#
# Node is needed only to build the UI; the image that runs holds one static
# binary.

FROM --platform=$BUILDPLATFORM node:24 AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN mkdir -p ../internal/webui/dist && npm run build

FROM --platform=$BUILDPLATFORM golang:1.27 AS build
ARG TARGETOS TARGETARCH
# VERSION names the build, as /api/v1/version reports it.
ARG VERSION=dev
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY cmd/ cmd/
COPY internal/ internal/
COPY --from=web /src/internal/webui/dist/app internal/webui/dist/app
RUN CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH \
    go build -trimpath -ldflags="-s -w -X github.com/csnewman/hangar/internal/version.Version=$VERSION" -o /hangar-server ./cmd/hangar-server

FROM gcr.io/distroless/static-debian13:nonroot
COPY --from=build /hangar-server /hangar-server
EXPOSE 8081
ENTRYPOINT ["/hangar-server"]

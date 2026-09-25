# syntax=docker/dockerfile:1.7
#
# vgames API image (08-release §1, A5-T07). Owner: Agent 5. Build from the repository root:
#   docker build -f apps/api/Dockerfile -t vgames-api .
#
# - The admin web (Node) and the API (Rust) are built from source; the image serves both.
# - Runtime: distroless (glibc + CA certificates, no shell, no package manager), non-root
#   user, nothing written to the image's filesystem (run with --read-only; only the fs
#   storage backend, which is for development, needs a mounted volume).
# - Base images are pinned by digest; Dependabot proposes digest updates.
# - The Rust toolchain is the one in the pinned image (RUSTUP_TOOLCHAIN), so a build never
#   downloads a newer compiler.

ARG NODE_IMAGE=docker.io/library/node:22-bookworm-slim@sha256:43ac6c60b8f89723f746e8a92ce91abd5017e627ce1ddfe4238355d3a30b772c
ARG RUST_IMAGE=docker.io/library/rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

FROM ${NODE_IMAGE} AS admin-web
WORKDIR /src
ENV CI=true COREPACK_ENABLE_DOWNLOAD_PROMPT=0
RUN corepack enable
COPY . .
RUN pnpm install --frozen-lockfile --filter "@vgames/admin-web..." \
    && pnpm --filter @vgames/admin-web build

FROM ${RUST_IMAGE} AS api
WORKDIR /src
ENV CARGO_TERM_COLOR=always CARGO_INCREMENTAL=0 SQLX_OFFLINE=true RUSTUP_TOOLCHAIN=1.98.1
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --locked --release -p vgames-api \
    && cp target/release/vgames-api /vgames-api

FROM ${RUNTIME_IMAGE}
LABEL org.opencontainers.image.title="vgames-api" \
      org.opencontainers.image.description="vgames server: API, realtime gateway, jobs and the admin web"
COPY --from=api /vgames-api /usr/local/bin/vgames-api
COPY --from=admin-web /src/apps/admin-web/dist /srv/admin
ENV VGAMES_ADMIN_DIST=/srv/admin \
    VGAMES_BIND_ADDR=0.0.0.0:8080 \
    VGAMES_LOG_FORMAT=json
USER nonroot:nonroot
EXPOSE 8080
# Liveness/readiness: GET /v1/health (probe from the orchestrator; there is no shell here).
ENTRYPOINT ["/usr/local/bin/vgames-api"]

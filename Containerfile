# syntax=docker/dockerfile:1
#
# Production image: a static musl `supp` binary and the built web client on
# `scratch`. See "Deploying with Dokku" in README.md.

ARG RUST_IMAGE=rust:1.97-alpine
ARG TRUNK_VERSION=0.21.14
# Dokku's `storage:ensure-directory` chowns to 32767 by default; match it so
# the mounted /data is writable.
ARG APP_UID=32767

# --- Web client (wasm; the output is platform independent) ---
FROM --platform=$BUILDPLATFORM ${RUST_IMAGE} AS web
ARG TRUNK_VERSION
# The rust:alpine image already has musl-dev and gcc.
RUN rustup target add wasm32-unknown-unknown \
 && wget -qO- "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-$(uname -m)-unknown-linux-musl.tar.gz" \
    | tar -xz -C /usr/local/bin trunk
WORKDIR /src
COPY . .
# trunk fetches the wasm-bindgen and wasm-opt versions it needs into its cache.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=supp-target-web,target=/src/target \
    --mount=type=cache,target=/root/.cache/trunk \
    cd crates/web && trunk build --release --locked

# --- Server ---
FROM ${RUST_IMAGE} AS server
ARG APP_UID
# The runtime's whole filesystem besides the app: a `supp` user, its data
# directory and a /tmp for SQLite.
RUN mkdir -p /rootfs/etc /rootfs/data /rootfs/tmp \
 && echo "supp:x:${APP_UID}:${APP_UID}:supp:/data:/sbin/nologin" > /rootfs/etc/passwd \
 && echo "supp:x:${APP_UID}:" > /rootfs/etc/group \
 && chown ${APP_UID}:${APP_UID} /rootfs/data \
 && chmod 1777 /rootfs/tmp
WORKDIR /src
COPY . .
# The musl target links statically by default, bundled SQLite included.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=supp-target-server,target=/src/target \
    cargo build --release --locked -p supp-server \
 && cp target/release/supp /supp \
 && strip /supp

# --- Runtime ---
FROM scratch
COPY --from=server /rootfs /
COPY --from=server /supp /supp
COPY --from=web /src/crates/web/dist /static
ENV SUPP_LISTEN=0.0.0.0:5000 \
    SUPP_DB=/data/supp.db \
    SUPP_STATIC_DIR=/static
USER supp
VOLUME /data
EXPOSE 5000
# `supp serve` shuts down gracefully on SIGINT, not SIGTERM.
STOPSIGNAL SIGINT
ENTRYPOINT ["/supp"]
CMD ["serve"]

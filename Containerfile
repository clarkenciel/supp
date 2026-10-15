# syntax=docker/dockerfile:1
#
# Production image: a static musl `supp` binary and the built web client on
# `scratch`. See "Deploying with Dokku" in README.md.

ARG RUST_IMAGE=rust:1.97-alpine
ARG TRUNK_VERSION=0.21.14

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
WORKDIR /src
COPY . .
# The musl target links statically by default, bundled SQLite included.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=supp-target-server,target=/src/target \
    cargo build --release --locked -p supp-server \
 && cp target/release/supp /supp \
 && strip /supp \
 && mkdir -p /rootfs/data /rootfs/tmp \
 && chmod 1777 /rootfs/tmp

# --- Runtime ---
FROM scratch
# 32767 is the uid `dokku storage:ensure-directory` chowns to by default.
COPY --from=server --chown=32767:32767 /rootfs /
COPY --from=server /supp /supp
COPY --from=web /src/crates/web/dist /static
ENV SUPP_LISTEN=0.0.0.0:5000 \
    SUPP_DB=/data/supp.db \
    SUPP_STATIC_DIR=/static
USER 32767:32767
VOLUME /data
EXPOSE 5000
# `supp serve` shuts down gracefully on SIGINT, not SIGTERM.
STOPSIGNAL SIGINT
ENTRYPOINT ["/supp"]
CMD ["serve"]

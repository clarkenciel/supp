# Small, static `supp` image: a musl-linked server on FROM scratch, serving
# the trunk-built web client. Follows https://kerkour.com/rust-docker.
#
#   docker build -t supp -f Containerfile .        (or: podman build -t supp .)
#   docker run --rm -p 3000:3000 -v supp-data:/data \
#     -e SUPP_PUBLIC_URL=https://lists.example.com -e SUPP_AUTH=google \
#     -e SUPP_GOOGLE_CLIENT_ID=... -e SUPP_GOOGLE_CLIENT_SECRET=... supp
#   docker run --rm -v supp-data:/data supp admin invite create \
#     --public-url https://lists.example.com

####################################################################################################
## Web client (static files only, so the base image doesn't matter)
####################################################################################################
FROM rust:1-slim AS web

ARG TRUNK_VERSION=0.21.14

# binaryen provides wasm-opt for the release build's `data-wasm-opt`.
RUN apt-get update && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends binaryen \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown

WORKDIR /app
COPY Cargo.toml Cargo.lock ./

# trunk requires wasm-bindgen-cli to match the wasm-bindgen in Cargo.lock;
# installing both here keeps trunk from downloading tools at build time.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install --locked trunk --version "${TRUNK_VERSION}" \
    && cargo install --locked wasm-bindgen-cli --version "$(sed -n \
        '/^name = "wasm-bindgen"$/{n;s/^version = "\(.*\)"$/\1/p;}' Cargo.lock)"

COPY proto proto
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cd crates/web && trunk build --release --locked

####################################################################################################
## Server (statically linked against musl)
####################################################################################################
FROM rust:1-alpine AS builder

RUN apk update && \
    apk upgrade --no-cache && \
    apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY proto proto
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked -p supp-server \
    && cp target/release/supp /supp

####################################################################################################
## Files for the final image
####################################################################################################
FROM alpine:latest AS files

# tzdata is used for timezones info
RUN apk update && \
    apk upgrade --no-cache && \
    apk add --no-cache ca-certificates tzdata

RUN update-ca-certificates

ENV USER=supp
ENV UID=10001
RUN adduser \
    --disabled-password \
    --gecos "" \
    --home "/nonexistent" \
    --shell "/sbin/nologin" \
    --no-create-home \
    --uid "${UID}" \
    "${USER}"

# The database lives on a volume at /data; SQLite may also spill temp files.
RUN mkdir /data

####################################################################################################
## Final image
####################################################################################################
FROM scratch

COPY --from=files --chmod=444 /etc/passwd /etc/group /etc/
COPY --from=files --chmod=444 /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=files /usr/share/zoneinfo /usr/share/zoneinfo
COPY --from=files --chmod=1777 /tmp /tmp
COPY --from=files --chown=supp:supp /data /data

COPY --from=web /app/crates/web/dist /usr/share/supp/web
COPY --from=builder /supp /bin/supp

ENV SUPP_DB=/data/supp.db \
    SUPP_LISTEN=0.0.0.0:3000 \
    SUPP_STATIC_DIR=/usr/share/supp/web

# Use an unprivileged user.
USER supp:supp

WORKDIR /data
VOLUME /data
EXPOSE 3000

ENTRYPOINT ["/bin/supp"]
CMD ["serve"]

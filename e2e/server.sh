#!/bin/sh
# Builds the client and server, then runs the server the way production
# does (one origin, static client) but with the dev identity provider and a
# fresh database.
set -e
cd "$(dirname "$0")/.."
cargo build -q -p supp-server
(cd crates/web && trunk build)
rm -rf e2e/.data && mkdir -p e2e/.data
exec target/debug/supp --db e2e/.data/supp.db serve \
  --auth dev \
  --listen 127.0.0.1:3100 \
  --public-url http://127.0.0.1:3100 \
  --static-dir crates/web/dist

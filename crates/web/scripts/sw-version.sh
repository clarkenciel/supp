#!/bin/sh
# trunk post_build hook: writes this build's file list and a version hash
# into the staged sw.js (see the comment at the top of sw.js).
#
# The version hashes every file's name and contents, so it changes exactly
# when anything the worker serves changes. Under `trunk serve` (detected by
# the live-reload placeholder trunk puts in index.html) the worker is left
# as the no-cache 'dev' version, so rebuilds show up on reload without an
# update prompt.
set -eu
cd "$TRUNK_STAGING_DIR"
if grep -q '__TRUNK_ADDRESS__' index.html; then
  exit 0
fi

files=$(find . -type f ! -name sw.js | sed 's|^\./||' | LC_ALL=C sort)
if command -v sha256sum >/dev/null; then
  sha256() { sha256sum; }
else
  sha256() { shasum -a 256; }
fi
version=$(for f in $files; do printf '%s\n' "$f"; cat "$f"; done | sha256 | cut -c1-16)
assets=$(for f in $files; do printf "'%s', " "$f"; done)

sed -e "s|^const VERSION = .*|const VERSION = '$version';|" \
    -e "s|^const ASSETS = .*|const ASSETS = [${assets%, }];|" \
    sw.js > sw.js.tmp
mv sw.js.tmp sw.js

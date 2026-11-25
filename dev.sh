#!/usr/bin/env bash
# Runs ansel locally, rebuilding on every change.
set -euo pipefail
cd "$(dirname "$0")"

if [[ ! -f .env ]]; then
  token=$(openssl rand -hex 32 2>/dev/null || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
  cat > .env <<ENV
# Local development settings. This file is gitignored.
ANSEL_API_TOKEN=${token}

# Store images on disk instead of in a bucket.
ANSEL_STORAGE=fs
ANSEL_FS_ROOT=./data

# To test against a bucket instead, remove ANSEL_STORAGE and set:
# BUCKET=
# ENDPOINT=
# REGION=
# ACCESS_KEY_ID=
# SECRET_ACCESS_KEY=

# For the client SDK.
ANSEL_URL=http://localhost:8080

RUST_LOG=ansel=debug
ENV
  echo "Created .env with a new ANSEL_API_TOKEN."
fi

if ! command -v cargo-watch >/dev/null; then
  echo "cargo-watch is not installed. Install it with: cargo install cargo-watch" >&2
  exit 1
fi

exec cargo watch -x "run --no-default-features --features server"

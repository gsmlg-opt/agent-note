#!/usr/bin/env bash
set -euo pipefail
fixture_dir="$(cd "$(dirname "$0")" && pwd)"
repo_dir="$(cd "$fixture_dir/../../../../.." && pwd)"
cd "$fixture_dir"
mix deps.get
mix compile
mix test --no-start
cd "$repo_dir"
cargo test --locked -p note-server --lib sync_client::tests::real_phoenix -- --ignored --nocapture

#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${1:-}" == "--check" ]]; then
  cargo fmt --manifest-path "$repo_dir/rust/Cargo.toml" --all -- --check
else
  cargo fmt --manifest-path "$repo_dir/rust/Cargo.toml" --all
fi


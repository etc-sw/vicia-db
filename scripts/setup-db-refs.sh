#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ref_root="${DB_REF_DIR:-$HOME/db-ref}"
tool_dir="$repo_root/tools/ref-db-bench"

mkdir -p "$ref_root"

clone_or_update() {
  local name="$1"
  local url="$2"
  local checkout="$ref_root/$name"
  if [[ -d "$checkout/.git" ]]; then
    git -C "$checkout" fetch --prune origin
  else
    git clone --filter=blob:none "$url" "$checkout"
  fi
  printf '%-8s %s\n' "$name" "$(git -C "$checkout" rev-parse HEAD)"
}

clone_or_update grafeo https://github.com/GrafeoDB/grafeo.git
clone_or_update redb https://github.com/cberner/redb.git
clone_or_update fjall https://github.com/fjall-rs/fjall.git
clone_or_update turso https://github.com/tursodatabase/turso.git
clone_or_update cozo https://github.com/cozodb/cozo.git

escaped_ref_root="${ref_root//|/\\|}"
sed "s|@DB_REF_DIR@|$escaped_ref_root|g" \
  "$tool_dir/Cargo.toml.in" >"$tool_dir/Cargo.toml"

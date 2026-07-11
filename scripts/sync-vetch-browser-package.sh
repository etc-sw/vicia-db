#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
requested_vetch_dir="${1:-}"

if [[ -n "$requested_vetch_dir" ]]; then
  vetch_dir="$(cd "$requested_vetch_dir" && pwd)"
else
  common_dir="$(git -C "$repo_root" rev-parse --path-format=absolute --git-common-dir)"
  primary_checkout="$(dirname "$common_dir")"
  vetch_dir="$(dirname "$primary_checkout")/vetch-app"
fi

quiet_surface="$vetch_dir/apps/quiet-surface"
destination="$quiet_surface/vendor/vicia-browser"

if [[ ! -f "$quiet_surface/package.json" ]]; then
  echo "error: Vetch quiet-surface package not found at $quiet_surface" >&2
  echo "usage: just sync [VETCH_APP_DIR]" >&2
  exit 1
fi

if ! grep -q '"@vicia-db/browser"' "$quiet_surface/package.json"; then
  echo "error: $quiet_surface/package.json does not declare @vicia-db/browser" >&2
  exit 1
fi

command -v wasm-pack >/dev/null || {
  echo "error: wasm-pack is required" >&2
  exit 1
}
command -v pnpm >/dev/null || {
  echo "error: pnpm is required" >&2
  exit 1
}

stage_root="$(mktemp -d "${TMPDIR:-/tmp}/vicia-browser-sync.XXXXXX")"
trap 'rm -rf "$stage_root"' EXIT
stage_package="$stage_root/package"

wasm-pack build \
  --target web \
  --scope vicia-db \
  --out-name vicia_db \
  --out-dir "$stage_package" \
  "$repo_root/bindings/browser" \
  --features browser

for required in package.json vicia_db.js vicia_db.d.ts vicia_db_bg.wasm; do
  if [[ ! -f "$stage_package/$required" ]]; then
    echo "error: wasm-pack did not produce $required" >&2
    exit 1
  fi
done

# wasm-pack ignores its own output by default. The Vetch-local package is an
# intentional, reviewable build artifact, so remove that generated ignore file.
rm -f "$stage_package/.gitignore"
cp "$repo_root/LICENSE-MIT" "$stage_package/LICENSE-MIT"
cp "$repo_root/LICENSE-APACHE" "$stage_package/LICENSE-APACHE"

source_commit="$(git -C "$repo_root" rev-parse HEAD)"
source_dirty=false
if [[ -n "$(git -C "$repo_root" status --porcelain --untracked-files=normal)" ]]; then
  source_dirty=true
fi
wasm_sha256="$(sha256sum "$stage_package/vicia_db_bg.wasm" | awk '{print $1}')"
wasm_pack_version="$(wasm-pack --version | awk '{print $2}')"

SOURCE_COMMIT="$source_commit" \
SOURCE_DIRTY="$source_dirty" \
WASM_SHA256="$wasm_sha256" \
WASM_PACK_VERSION="$wasm_pack_version" \
node - "$stage_package" <<'NODE'
const fs = require("node:fs");
const path = require("node:path");

const packageDir = process.argv[2];
const packagePath = path.join(packageDir, "package.json");
const pkg = JSON.parse(fs.readFileSync(packagePath, "utf8"));
pkg.name = "@vicia-db/browser";
pkg.description = "Vicia DB browser WebAssembly package";
pkg.repository = {
  type: "git",
  url: "https://github.com/etc-sw/vicia-db.git",
};
pkg.files = [
  ...new Set([
    ...(pkg.files ?? []),
    "vicia-build.json",
    "LICENSE-MIT",
    "LICENSE-APACHE",
  ]),
];
fs.writeFileSync(packagePath, `${JSON.stringify(pkg, null, 2)}\n`);

const provenance = {
  package: pkg.name,
  version: pkg.version,
  sourceCommit: process.env.SOURCE_COMMIT,
  sourceDirty: process.env.SOURCE_DIRTY === "true",
  wasmSha256: process.env.WASM_SHA256,
  wasmPackVersion: process.env.WASM_PACK_VERSION,
};
fs.writeFileSync(
  path.join(packageDir, "vicia-build.json"),
  `${JSON.stringify(provenance, null, 2)}\n`,
);
NODE

mkdir -p "$(dirname "$destination")"
next_destination="${destination}.next.$$"
rm -rf "$next_destination"
mv "$stage_package" "$next_destination"
rm -rf "$destination"
mv "$next_destination" "$destination"

pnpm --dir "$quiet_surface" install --prefer-offline

resolved_package="$quiet_surface/node_modules/@vicia-db/browser/package.json"
if [[ ! -f "$resolved_package" ]]; then
  echo "error: pnpm did not link @vicia-db/browser into quiet-surface" >&2
  exit 1
fi

echo "synced @vicia-db/browser from $source_commit"
echo "wasm sha256: $wasm_sha256"
echo "destination: $destination"

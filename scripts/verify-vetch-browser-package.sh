#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
candidate="$(cd "${1:?candidate package directory is required}" && pwd)"
vetch_dir="$(cd "${2:?Vetch repository directory is required}" && pwd)"
receipt="${3:?receipt output path is required}"
quiet_surface="$vetch_dir/apps/quiet-surface"
checks_file="$(mktemp "${TMPDIR:-/tmp}/vicia-integration-checks.XXXXXX")"
linked_package="$quiet_surface/node_modules/@vicia-db/browser"
original_link=""

cleanup() {
  if [[ -n "$original_link" ]]; then
    rm -f "$linked_package"
    ln -s "$original_link" "$linked_package"
  fi
  rm -f "$checks_file"
}
trap cleanup EXIT

find_chromedriver() {
  if [[ -n "${CHROMEDRIVER:-}" && -x "$CHROMEDRIVER" ]]; then
    printf '%s\n' "$CHROMEDRIVER"
    return
  fi

  local chrome_major=""
  if command -v google-chrome >/dev/null; then
    chrome_major="$(google-chrome --version | sed -E 's/.* ([0-9]+)\..*/\1/')"
  elif command -v chromium >/dev/null; then
    chrome_major="$(chromium --version | sed -E 's/.* ([0-9]+)\..*/\1/')"
  fi

  local driver
  while IFS= read -r driver; do
    if [[ -z "$chrome_major" || "$driver" == *"/$chrome_major."* ]]; then
      printf '%s\n' "$driver"
      return
    fi
  done < <(find "$HOME/.cache/chrome-for-testing" -path '*/chromedriver-linux64/chromedriver' -type f -executable 2>/dev/null | sort -Vr)

  echo "error: compatible ChromeDriver not found; set CHROMEDRIVER" >&2
  return 1
}

write_receipt() {
  local status="$1"
  mkdir -p "$(dirname "$receipt")"
  SOURCE_COMMIT="${SOURCE_COMMIT:?}" \
  SOURCE_DIRTY="${SOURCE_DIRTY:?}" \
  WASM_SHA256="${WASM_SHA256:?}" \
  WASM_PACK_VERSION="${WASM_PACK_VERSION:?}" \
  VETCH_COMMIT="$(git -C "$vetch_dir" rev-parse HEAD)" \
  VETCH_DIRTY="$([[ -n "$(git -C "$vetch_dir" status --porcelain --untracked-files=normal)" ]] && echo true || echo false)" \
  INTEGRATION_STATUS="$status" \
  node - "$checks_file" "$receipt" <<'NODE'
const fs = require("node:fs");

const [checksPath, receiptPath] = process.argv.slice(2);
const checks = fs.readFileSync(checksPath, "utf8")
  .split("\n")
  .filter(Boolean)
  .map((line) => {
    const [name, status, durationMs] = line.split("\t");
    return { name, status, durationMs: Number(durationMs) };
  });

const value = {
  schemaVersion: 1,
  status: process.env.INTEGRATION_STATUS,
  generatedAt: new Date().toISOString(),
  vicia: {
    sourceCommit: process.env.SOURCE_COMMIT,
    sourceDirty: process.env.SOURCE_DIRTY === "true",
    wasmSha256: process.env.WASM_SHA256,
    wasmPackVersion: process.env.WASM_PACK_VERSION,
  },
  vetch: {
    sourceCommit: process.env.VETCH_COMMIT,
    sourceDirty: process.env.VETCH_DIRTY === "true",
  },
  checks,
};
fs.writeFileSync(receiptPath, `${JSON.stringify(value, null, 2)}\n`);
NODE
}

run_check() {
  local name="$1"
  shift
  local started ended duration
  started="$(date +%s%3N)"
  if "$@"; then
    ended="$(date +%s%3N)"
    duration="$((ended - started))"
    printf '%s\tpassed\t%s\n' "$name" "$duration" >> "$checks_file"
  else
    ended="$(date +%s%3N)"
    duration="$((ended - started))"
    printf '%s\tfailed\t%s\n' "$name" "$duration" >> "$checks_file"
    write_receipt failed
    echo "error: integration check failed: $name" >&2
    exit 1
  fi
}

for required in package.json vicia_db.js vicia_db.d.ts vicia_db_bg.wasm vicia-build.json; do
  [[ -f "$candidate/$required" ]] || {
    echo "error: candidate package is missing $required" >&2
    exit 1
  }
done

chromedriver="$(find_chromedriver)"

run_check pnpm-install pnpm --dir "$quiet_surface" install --prefer-offline

if [[ ! -L "$linked_package" ]]; then
  echo "error: expected pnpm package link at $linked_package" >&2
  exit 1
fi
original_link="$(readlink "$linked_package")"
rm "$linked_package"
ln -s "$candidate" "$linked_package"

run_check browser-format-matrix env CHROMEDRIVER="$chromedriver" "$repo_root/scripts/test-browser-wasm.sh"
run_check authority-spike env VICIA_BROWSER_PACKAGE_DIR="$candidate" pnpm --dir "$quiet_surface" smoke:vicia-authority-spike
run_check authority-contract env VICIA_BROWSER_PACKAGE_DIR="$candidate" pnpm --dir "$quiet_surface" smoke:vicia-authority-contract
run_check authority-lifecycle env VICIA_BROWSER_PACKAGE_DIR="$candidate" pnpm --dir "$quiet_surface" smoke:vicia-authority-lifecycle
run_check canvas-persistence-concurrency env VICIA_BROWSER_PACKAGE_DIR="$candidate" pnpm --dir "$quiet_surface" smoke:canvas-persistence-concurrency
run_check typecheck env VICIA_BROWSER_PACKAGE_DIR="$candidate" pnpm --dir "$quiet_surface" typecheck
run_check build env VICIA_BROWSER_PACKAGE_DIR="$candidate" pnpm --dir "$quiet_surface" build

write_receipt passed

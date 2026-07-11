#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

command -v cargo >/dev/null || {
  echo "error: cargo is required" >&2
  exit 1
}
command -v jq >/dev/null || {
  echo "error: jq is required" >&2
  exit 1
}
command -v wasm-bindgen-test-runner >/dev/null || {
  echo "error: wasm-bindgen-test-runner is required" >&2
  exit 1
}
if [[ -z "${CHROMEDRIVER:-}" || ! -x "$CHROMEDRIVER" ]]; then
  echo "error: CHROMEDRIVER must point to a compatible executable" >&2
  exit 1
fi

expected_version="$(
  cargo tree --target wasm32-unknown-unknown -p wasm-bindgen --depth 0 --prefix none \
    | awk 'NR == 1 { sub(/^v/, "", $2); print $2 }'
)"
runner_version="$(wasm-bindgen-test-runner --version | awk '{print $2}')"
if [[ "$runner_version" != "$expected_version" ]]; then
  echo "error: wasm-bindgen-test-runner $runner_version does not match crate $expected_version" >&2
  exit 1
fi

artifact="$({
  cargo test \
    --target wasm32-unknown-unknown \
    --lib \
    --features browser \
    --no-run \
    --message-format=json
} | jq -r '
  select(
    .reason == "compiler-artifact"
    and .profile.test == true
    and .target.name == "minigraf"
    and (.target.crate_types | index("rlib"))
    and .executable != null
  )
  | .executable
' | tail -n 1)"

if [[ -z "$artifact" || ! -f "$artifact" ]]; then
  echo "error: browser WASM test artifact was not produced" >&2
  exit 1
fi

WASM_BINDGEN_TEST_ONLY_WEB=1 \
WASM_BINDGEN_TEST_TIMEOUT="${WASM_BINDGEN_TEST_TIMEOUT:-300}" \
wasm-bindgen-test-runner "$artifact" --nocapture

# Vicia DB local development task surface.

# list recipes
default:
    @just --list

# Build the browser WASM package from this checkout and atomically install it
# into Vetch's local @vicia-db/browser package boundary.
#
# Default target: sibling ~/projects/vetch-app checkout.
# Worktree verification: just sync /absolute/path/to/vetch-worktree
sync VETCH_APP_DIR="":
    ./scripts/sync-vetch-browser-package.sh "{{VETCH_APP_DIR}}"

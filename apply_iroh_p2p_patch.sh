#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 /path/to/rusternetes" >&2
  exit 1
fi

RUSTERNETES_DIR="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="$SCRIPT_DIR/rusternetes-iroh-overlay.diff"

if [[ ! -d "$RUSTERNETES_DIR" ]]; then
  echo "Error: directory not found: $RUSTERNETES_DIR" >&2
  exit 1
fi

if [[ ! -f "$PATCH_FILE" ]]; then
  echo "Error: patch file not found: $PATCH_FILE" >&2
  exit 1
fi

cd "$RUSTERNETES_DIR"

git apply "$PATCH_FILE"

echo "Patch applied successfully."

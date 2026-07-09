#!/usr/bin/env bash
# Install tokmeter binary + desktop entry for the current user.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
DESKTOP_SRC="${ROOT}/assets/tokmeter.desktop"
DESKTOP_DST="${APP_DIR}/tokmeter.desktop"

mkdir -p "$BIN_DIR" "$APP_DIR"

echo "Building release…"
cargo build --release

install -m 755 "${ROOT}/target/release/tokmeter" "${BIN_DIR}/tokmeter"
echo "Installed binary → ${BIN_DIR}/tokmeter"

# Point Exec at the absolute path so PATH is not required by the desktop session.
sed "s|^Exec=tokmeter$|Exec=${BIN_DIR}/tokmeter|" "$DESKTOP_SRC" >"$DESKTOP_DST"
chmod 644 "$DESKTOP_DST"
echo "Installed desktop entry → ${DESKTOP_DST}"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" 2>/dev/null || true
fi

echo "Done. Launch from the app menu or: ${BIN_DIR}/tokmeter"

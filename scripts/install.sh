#!/usr/bin/env bash
# Install tokmeter binary, icons, and desktop entry for the current user.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_BASE="${HOME}/.local/share/icons/hicolor"
DESKTOP_SRC="${ROOT}/assets/tokmeter.desktop"
DESKTOP_DST="${APP_DIR}/tokmeter.desktop"

mkdir -p "$BIN_DIR" "$APP_DIR"

echo "Building release…"
cargo build --release

install -m 755 "${ROOT}/target/release/tokmeter" "${BIN_DIR}/tokmeter"
echo "Installed binary → ${BIN_DIR}/tokmeter"

# Icons (KDE/Plasma picks these by Icon=tokmeter when app_id matches).
if [[ -f "${ROOT}/assets/tokmeter.svg" ]]; then
  mkdir -p "${ICON_BASE}/scalable/apps"
  install -m 644 "${ROOT}/assets/tokmeter.svg" "${ICON_BASE}/scalable/apps/tokmeter.svg"
  echo "Installed SVG icon → ${ICON_BASE}/scalable/apps/tokmeter.svg"
fi
for s in 16 24 32 48 64 128 256; do
  src="${ROOT}/assets/tokmeter-${s}.png"
  if [[ -f "$src" ]]; then
    mkdir -p "${ICON_BASE}/${s}x${s}/apps"
    install -m 644 "$src" "${ICON_BASE}/${s}x${s}/apps/tokmeter.png"
  fi
done
echo "Installed PNG icons → ${ICON_BASE}/*/apps/tokmeter.png"

# Point Exec at the absolute path so PATH is not required by the desktop session.
sed "s|^Exec=tokmeter$|Exec=${BIN_DIR}/tokmeter|" "$DESKTOP_SRC" >"$DESKTOP_DST"
chmod 644 "$DESKTOP_DST"
echo "Installed desktop entry → ${DESKTOP_DST}"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t "${ICON_BASE}" 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" 2>/dev/null || true
fi
# KDE/Plasma often needs a cache refresh for new icons.
if command -v kbuildsycoca6 >/dev/null 2>&1; then
  kbuildsycoca6 --noincremental 2>/dev/null || true
elif command -v kbuildsycoca5 >/dev/null 2>&1; then
  kbuildsycoca5 --noincremental 2>/dev/null || true
fi

echo "Done. Launch: ${BIN_DIR}/tokmeter  (or from app menu)"
echo "Tip: if the taskbar still shows a generic icon, log out/in or restart plasmashell once."

#!/usr/bin/env bash

set -Eeuo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# Keep the build independent of the caller's PATH and of FUSE2 availability.
export PATH="${HOME}/.cargo/bin:${PATH}"
export OPENSSL_NO_VENDOR="${OPENSSL_NO_VENDOR:-1}"
export APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}"
# Use the installed pnpm version even though package.json pins another version.
export PNPM_CONFIG_MANAGE_PACKAGE_MANAGER_VERSIONS="${PNPM_CONFIG_MANAGE_PACKAGE_MANAGER_VERSIONS:-false}"

TAURI_CACHE_DIR="${TAURI_CACHE_DIR:-${HOME}/.cache/tauri}"
LINUXDEPLOY="${LINUXDEPLOY:-${TAURI_CACHE_DIR}/linuxdeploy-x86_64.AppImage}"
APPIMAGE_PLUGIN="${APPIMAGE_PLUGIN:-${TAURI_CACHE_DIR}/linuxdeploy-plugin-appimage.AppImage}"
PNPM_BIN="${PNPM_BIN:-$(command -v pnpm || true)}"

PACKAGE_NAME="$(node -p "require('./package.json').name")"
VERSION="$(node -p "require('./package.json').version")"
BUNDLE_DIR="${ROOT_DIR}/src-tauri/target/release/bundle/appimage"
APPDIR="${BUNDLE_DIR}/${PACKAGE_NAME}.AppDir"
RAW_APPIMAGE="${BUNDLE_DIR}/${PACKAGE_NAME}-x86_64.AppImage"
OUTPUT="${BUNDLE_DIR}/${PACKAGE_NAME}_${VERSION}_amd64.AppImage"

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ -x "$LINUXDEPLOY" ]] || die "linuxdeploy not found or not executable: $LINUXDEPLOY"
[[ -x "$APPIMAGE_PLUGIN" ]] || die "linuxdeploy AppImage plugin not found or not executable: $APPIMAGE_PLUGIN"
[[ -n "$PNPM_BIN" && -x "$PNPM_BIN" ]] || die "pnpm not found; set PNPM_BIN to the installed pnpm executable"

printf '==> Building %s %s\n' "$PACKAGE_NAME" "$VERSION"
printf '    root: %s\n' "$ROOT_DIR"

# Do not let a failed compile accidentally package an AppDir from an older run.
rm -rf "$APPDIR"
rm -f "$RAW_APPIMAGE" "$OUTPUT"

# Tauri runs the frontend build and the optimized Rust build, then prepares
# the AppDir even when its bundled linuxdeploy cannot parse a RELR section.
set +e
"$PNPM_BIN" tauri build --bundles appimage
TAURI_STATUS=$?
set -e

if [[ "$TAURI_STATUS" -ne 0 ]]; then
  printf '==> tauri build exited with %s; checking the generated AppDir for manual packaging\n' "$TAURI_STATUS"
fi

[[ -x "${APPDIR}/usr/bin/${PACKAGE_NAME}" ]] || {
  printf 'tauri build output:\n'
  printf '  expected executable: %s\n' "${APPDIR}/usr/bin/${PACKAGE_NAME}"
  die "Tauri did not produce a usable AppDir"
}

mkdir -p "$BUNDLE_DIR"
rm -f "$RAW_APPIMAGE" "$OUTPUT"

printf '==> Packaging AppDir with linuxdeploy-plugin-appimage\n'
(
  cd "$BUNDLE_DIR"
  "$APPIMAGE_PLUGIN" --appdir "$APPDIR"
)

[[ -f "$RAW_APPIMAGE" ]] || die "AppImage plugin did not create $RAW_APPIMAGE"
mv -- "$RAW_APPIMAGE" "$OUTPUT"
chmod +x "$OUTPUT"

if [[ "$TAURI_STATUS" -ne 0 ]]; then
  printf '==> AppImage packaging completed after the known linuxdeploy fallback\n'
fi
printf '==> Output: %s\n' "$OUTPUT"
printf '==> SHA256: '
sha256sum "$OUTPUT"

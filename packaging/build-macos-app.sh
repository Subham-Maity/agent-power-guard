#!/usr/bin/env bash
set -euo pipefail

# macOS .app bundle & universal binary builder
APP_NAME="Agent Power Guard.app"
BUNDLE_DIR="dist/${APP_NAME}"
CONTENTS="${BUNDLE_DIR}/Contents"
MACOS="${CONTENTS}/MacOS"
RESOURCES="${CONTENTS}/Resources"

echo "Creating macOS Application Bundle: ${APP_NAME}..."

mkdir -p "${MACOS}"
mkdir -p "${RESOURCES}"

# Universal binary with lipo if both architectures exist, otherwise copy single arch
ARM_BIN="target/aarch64-apple-darwin/release/agent-power-guard"
INTEL_BIN="target/x86_64-apple-darwin/release/agent-power-guard"

if [ -f "${ARM_BIN}" ] && [ -f "${INTEL_BIN}" ]; then
    echo "Creating universal binary with lipo (arm64 + x86_64)..."
    lipo -create -output "${MACOS}/agent-power-guard" "${ARM_BIN}" "${INTEL_BIN}"
elif [ -f "${ARM_BIN}" ]; then
    echo "Using Apple Silicon (arm64) binary..."
    cp "${ARM_BIN}" "${MACOS}/agent-power-guard"
elif [ -f "${INTEL_BIN}" ]; then
    echo "Using Intel (x86_64) binary..."
    cp "${INTEL_BIN}" "${MACOS}/agent-power-guard"
elif [ -f "target/release/agent-power-guard" ]; then
    cp "target/release/agent-power-guard" "${MACOS}/agent-power-guard"
else
    echo "Error: No release binary found."
    exit 1
fi

chmod 755 "${MACOS}/agent-power-guard"
cp packaging/macos/Info.plist "${CONTENTS}/"
cp assets/icon_32.png "${RESOURCES}/app.png"
cp assets/icon_32.png "${RESOURCES}/app.icns" 2>/dev/null || true

# Zip the bundle for distribution
cd dist
zip -r "AgentPowerGuard-macos-universal.zip" "${APP_NAME}"
echo "macOS Application Bundle created at dist/AgentPowerGuard-macos-universal.zip"


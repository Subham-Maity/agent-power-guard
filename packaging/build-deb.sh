#!/usr/bin/env bash
set -euo pipefail

# Debian (.deb) package generator for Agent Power Guard
VERSION="1.3.0"
ARCH="amd64"
PKG_DIR="target/debian/agent-power-guard_${VERSION}_${ARCH}"

echo "Building Debian package for Agent Power Guard v${VERSION} (${ARCH})..."

mkdir -p "${PKG_DIR}/DEBIAN"
mkdir -p "${PKG_DIR}/usr/bin"
mkdir -p "${PKG_DIR}/usr/share/applications"
mkdir -p "${PKG_DIR}/usr/share/icons/hicolor/32x32/apps"
mkdir -p "${PKG_DIR}/usr/lib/systemd/user"

# Copy binary
if [ -f "target/release/agent-power-guard" ]; then
    cp target/release/agent-power-guard "${PKG_DIR}/usr/bin/"
elif [ -f "target/x86_64-unknown-linux-gnu/release/agent-power-guard" ]; then
    cp target/x86_64-unknown-linux-gnu/release/agent-power-guard "${PKG_DIR}/usr/bin/"
else
    echo "Error: Release binary not found. Run cargo build --release first."
    exit 1
fi
chmod 755 "${PKG_DIR}/usr/bin/agent-power-guard"

# Copy desktop entry, icons and systemd unit
cp packaging/agent-power-guard.desktop "${PKG_DIR}/usr/share/applications/"
cp assets/icon_32.png "${PKG_DIR}/usr/share/icons/hicolor/32x32/apps/agent-power-guard.png"
cp packaging/systemd/agent-power-guard.service "${PKG_DIR}/usr/lib/systemd/user/"

# Generate Debian control file
cat <<EOF > "${PKG_DIR}/DEBIAN/control"
Package: agent-power-guard
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Maintainer: Subham Maity <97989643+Subham-Maity@users.noreply.github.com>
Depends: libx11-6, libxext6, libxrandr2, libxinerama1, libxcursor1, libxi6, libxss1, xprintidle, libgtk-3-0, libayatana-appindicator3-1
Description: Native power watchdog for AI coding agents and human inactivity
 Agent Power Guard monitors user input, agent processes (Cursor, Claude,
 Antigravity), GPU compute loads, downloads, and power requests to sleep your PC
 only when both you and your agents are truly idle.
EOF

mkdir -p dist
dpkg-deb --build "${PKG_DIR}" "dist/agent-power-guard_${VERSION}_${ARCH}.deb"
echo "Debian package created at dist/agent-power-guard_${VERSION}_${ARCH}.deb"


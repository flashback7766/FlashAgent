#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
VERSION="${1:-}"
if [ -z "${VERSION}" ] || [ "${VERSION}" = "beta" ] || [ "${VERSION}" = "release" ] || [ "${VERSION}" = "stable" ] || [ "${VERSION}" = "latest" ]; then
    VERSION="$(git -C "${ROOT_DIR}" describe --tags --match 'b[0-9]*' --exact-match 2>/dev/null || git -C "${ROOT_DIR}" describe --tags --match 'b[0-9]*' 2>/dev/null || git -C "${ROOT_DIR}" describe --tags --match 'v*' 2>/dev/null || echo "b215")"
fi
RAW_VER="${VERSION#v}"
ARCH_VER="${RAW_VER//-/_}"
export FLASHAGENT_VERSION="${VERSION}"

echo "=== Building FlashAgent Release Packages (${VERSION}) ==="
mkdir -p "${DIST_DIR}"

# 1. Build release binary with explicit version embedded
BINARY="${ROOT_DIR}/target/release/flashagent-tui"
echo "--> Compiling release binary with FLASHAGENT_VERSION=${VERSION}..."
FLASHAGENT_VERSION="${VERSION}" cargo build --release --workspace -p flashagent-tui
strip "${BINARY}" || true

# Ensure icon and desktop file exist
mkdir -p "${ROOT_DIR}/packaging/desktop"
if [ ! -f "${ROOT_DIR}/packaging/desktop/flashagent.png" ] && command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w 256 -h 256 "${ROOT_DIR}/packaging/desktop/flashagent.svg" -o "${ROOT_DIR}/packaging/desktop/flashagent.png"
fi

# 2. Arch Linux package (.pkg.tar.zst)
echo "--> Packaging for Arch Linux (.pkg.tar.zst)..."
if command -v makepkg >/dev/null 2>&1; then
    mkdir -p "${ROOT_DIR}/packaging/arch/src"
    cp "${BINARY}" "${ROOT_DIR}/packaging/desktop/flashagent.desktop" "${ROOT_DIR}/packaging/desktop/flashagent.png" "${ROOT_DIR}/LICENSE" "${ROOT_DIR}/packaging/arch/src/"
    sed -i "s/^pkgver=.*/pkgver=${ARCH_VER}/" "${ROOT_DIR}/packaging/arch/PKGBUILD"
    sed -i "s/^pkgrel=.*/pkgrel=1/" "${ROOT_DIR}/packaging/arch/PKGBUILD"
    (cd "${ROOT_DIR}/packaging/arch" && makepkg -ef --nodeps --skipchecksums)
    mv "${ROOT_DIR}/packaging/arch/"*.pkg.tar.zst "${DIST_DIR}/"
    rm -rf "${ROOT_DIR}/packaging/arch/pkg" "${ROOT_DIR}/packaging/arch/src"
else
    # Direct creation of Arch package using tar and zstd (Ubuntu CI runner compatibility)
    ARCH_DIR="/tmp/flashagent-arch-build"
    rm -rf "${ARCH_DIR}"
    mkdir -p "${ARCH_DIR}/usr/bin" "${ARCH_DIR}/usr/share/applications" "${ARCH_DIR}/usr/share/icons/hicolor/256x256/apps" "${ARCH_DIR}/usr/share/licenses/flashagent-bin"
    cp "${BINARY}" "${ARCH_DIR}/usr/bin/flashagent-tui"
    ln -sf flashagent-tui "${ARCH_DIR}/usr/bin/flashagent"
    cp "${ROOT_DIR}/packaging/desktop/flashagent.desktop" "${ARCH_DIR}/usr/share/applications/"
    cp "${ROOT_DIR}/packaging/desktop/flashagent.png" "${ARCH_DIR}/usr/share/icons/hicolor/256x256/apps/"
    cp "${ROOT_DIR}/LICENSE" "${ARCH_DIR}/usr/share/licenses/flashagent-bin/LICENSE"
    
    FILE_SIZE=$(stat -c%s "${BINARY}")
    cat << EOF > "${ARCH_DIR}/.PKGINFO"
pkgname = flashagent-bin
pkgbase = flashagent-bin
pkgver = ${ARCH_VER}-1
pkgdesc = Fast, local-first autonomous AI coding agent in pure Rust
url = https://github.com/flashback7766/FlashAgent
builddate = $(date +%s)
packager = FlashAgent CI <https://github.com/flashback7766/FlashAgent>
size = ${FILE_SIZE}
arch = x86_64
license = MIT
provides = flashagent
provides = flashagent-tui
depend = gcc-libs
depend = glibc
EOF
    tar --zstd -cf "${DIST_DIR}/flashagent-bin-${ARCH_VER}-1-x86_64.pkg.tar.zst" -C "${ARCH_DIR}" .PKGINFO usr
    rm -rf "${ARCH_DIR}"
fi

# 3. Debian / Ubuntu package (.deb)
echo "--> Packaging for Debian / Ubuntu (.deb)..."
if command -v dpkg-deb >/dev/null 2>&1; then
    DEB_DIR="/tmp/flashagent-deb-build"
    rm -rf "${DEB_DIR}"
    mkdir -p "${DEB_DIR}/DEBIAN" "${DEB_DIR}/usr/bin" "${DEB_DIR}/usr/share/applications" "${DEB_DIR}/usr/share/icons/hicolor/256x256/apps" "${DEB_DIR}/usr/share/doc/flashagent"
    
    # Debian policy requires version numbers to start with a digit
    if [[ "${RAW_VER}" =~ ^[0-9] ]]; then
        DEB_VER="${RAW_VER}"
    else
        DEB_VER="0.1.0~${RAW_VER}"
    fi
    sed -e "s/^Version:.*/Version: ${DEB_VER}-1/" "${ROOT_DIR}/packaging/debian/control" > "${DEB_DIR}/DEBIAN/control"
    cp "${BINARY}" "${DEB_DIR}/usr/bin/flashagent-tui"
    ln -sf flashagent-tui "${DEB_DIR}/usr/bin/flashagent"
    cp "${ROOT_DIR}/packaging/desktop/flashagent.desktop" "${DEB_DIR}/usr/share/applications/"
    cp "${ROOT_DIR}/packaging/desktop/flashagent.png" "${DEB_DIR}/usr/share/icons/hicolor/256x256/apps/"
    cp "${ROOT_DIR}/LICENSE" "${DEB_DIR}/usr/share/doc/flashagent/copyright"
    dpkg-deb --build --root-owner-group "${DEB_DIR}" "${DIST_DIR}/flashagent_${RAW_VER}-1_amd64.deb"
    cp -f "${DIST_DIR}/flashagent_${RAW_VER}-1_amd64.deb" "${DIST_DIR}/flashagent_${RAW_VER}_amd64.deb"
    rm -rf "${DEB_DIR}"
fi

# 4. Void Linux package bundle (tarball + xbps template)
echo "--> Packaging for Void Linux (xbps bundle)..."
VOID_DIR="/tmp/flashagent-void"
rm -rf "${VOID_DIR}"
mkdir -p "${VOID_DIR}/bin" "${VOID_DIR}/share/applications" "${VOID_DIR}/share/icons" "${VOID_DIR}/xbps"
cp "${BINARY}" "${VOID_DIR}/bin/flashagent"
ln -sf flashagent "${VOID_DIR}/bin/flashagent-tui"
cp "${ROOT_DIR}/packaging/desktop/flashagent.desktop" "${VOID_DIR}/share/applications/"
cp "${ROOT_DIR}/packaging/desktop/flashagent.png" "${VOID_DIR}/share/icons/"
sed -e "s/^version=.*/version=${RAW_VER}/" "${ROOT_DIR}/packaging/void/template" > "${VOID_DIR}/xbps/template"
cp "${ROOT_DIR}/LICENSE" "${ROOT_DIR}/README.md" "${VOID_DIR}/"
tar -czf "${DIST_DIR}/flashagent-${VERSION}-void-linux-x86_64.tar.gz" -C /tmp flashagent-void
rm -rf "${VOID_DIR}"

# 5. Linux AppImage
echo "--> Packaging Linux AppImage..."
APPDIR="/tmp/flashagent.AppDir"
rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin" "${APPDIR}/usr/share/applications" "${APPDIR}/usr/share/icons/hicolor/256x256/apps"
cp "${ROOT_DIR}/packaging/appimage/AppRun" "${APPDIR}/AppRun"
chmod +x "${APPDIR}/AppRun"
cp "${BINARY}" "${APPDIR}/usr/bin/flashagent-tui"
ln -sf flashagent-tui "${APPDIR}/usr/bin/flashagent"
cp "${ROOT_DIR}/packaging/desktop/flashagent.desktop" "${APPDIR}/"
cp "${ROOT_DIR}/packaging/desktop/flashagent.png" "${APPDIR}/"
cp "${ROOT_DIR}/packaging/desktop/flashagent.png" "${APPDIR}/.DirIcon"

APPIMAGETOOL=""
if command -v appimagetool >/dev/null 2>&1; then
    APPIMAGETOOL="appimagetool"
elif [ -x /tmp/appimagetool ]; then
    APPIMAGETOOL="/tmp/appimagetool"
fi

if [ -n "${APPIMAGETOOL}" ]; then
    (ARCH=x86_64 "${APPIMAGETOOL}" --appimage-extract-and-run "${APPDIR}" "${DIST_DIR}/FlashAgent-${VERSION}-x86_64.AppImage" || ARCH=x86_64 "${APPIMAGETOOL}" "${APPDIR}" "${DIST_DIR}/FlashAgent-${VERSION}-x86_64.AppImage") || true
    chmod +x "${DIST_DIR}/FlashAgent-${VERSION}-x86_64.AppImage" 2>/dev/null || true
fi
rm -rf "${APPDIR}"

# 6. Generic Linux tarballs
echo "--> Packaging Generic Linux tarballs..."
TAR_TMP="/tmp/flashagent-tarball-build"
rm -rf "${TAR_TMP}"
mkdir -p "${TAR_TMP}"
cp "${BINARY}" "${TAR_TMP}/flashagent-tui"
cp "${BINARY}" "${TAR_TMP}/flashagent"
tar -czf "${DIST_DIR}/flashagent-${VERSION}-linux-x86_64.tar.gz" -C "${TAR_TMP}" flashagent flashagent-tui
cp "${DIST_DIR}/flashagent-${VERSION}-linux-x86_64.tar.gz" "${DIST_DIR}/flashagent-${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
rm -rf "${TAR_TMP}"

echo ""
echo "=== All Available Packages in ${DIST_DIR} ==="
ls -lh "${DIST_DIR}"

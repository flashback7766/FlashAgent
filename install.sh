#!/usr/bin/env bash
set -eu
if (set -o pipefail 2>/dev/null); then
  set -o pipefail
fi

# FlashAgent one-line installer
#
#   curl -fsSL https://raw.githubusercontent.com/flashback7766/FlashAgent/main/install.sh | bash
#
# FLASHAGENT_CHANNEL=beta   the latest beta build instead of the latest stable
# FLASHAGENT_VERSION=<tag>  one stable release by its tag, vX.Y.Z+bN
# INSTALL_DIR=<folder>      where to put the binary
REPO="flashback7766/FlashAgent"

# INSTALL_DIR or BIN_DIR if set; system-wide for root, user-local otherwise.
if [ -z "${INSTALL_DIR:-}" ]; then
  if [ -n "${BIN_DIR:-}" ]; then
    INSTALL_DIR="${BIN_DIR}"
  elif [ "$(id -u)" -eq 0 ]; then
    INSTALL_DIR="/usr/local/bin"
  else
    INSTALL_DIR="${HOME}/.local/bin"
  fi
fi

TARGET_VERSION="${FLASHAGENT_VERSION:-${VERSION:-}}"
TARGET_CHANNEL="${FLASHAGENT_CHANNEL:-${CHANNEL:-}}"

echo "⚡ Installing FlashAgent..."

# 1. Required tools. A script piped into bash does not install packages behind
# your back: it says what is missing and how to get it, then stops.
MISSING=""
if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
  MISSING="${MISSING} curl"
fi
for tool in tar gzip; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    MISSING="${MISSING} ${tool}"
  fi
done
# Alpine is musl-based; the release binary needs the glibc compatibility layer.
if [ -f /etc/alpine-release ]; then
  if ! apk info -e gcompat >/dev/null 2>&1 && [ ! -e /lib/ld-linux-x86-64.so.2 ]; then
    MISSING="${MISSING} gcompat"
  fi
fi

if [ -n "${MISSING}" ]; then
  echo "Error: FlashAgent's installer needs:${MISSING}"
  echo "Install them, then run this installer again:"
  if command -v apt-get >/dev/null 2>&1; then
    echo "  sudo apt-get install -y${MISSING}"
  elif command -v dnf >/dev/null 2>&1; then
    echo "  sudo dnf install -y${MISSING}"
  elif command -v yum >/dev/null 2>&1; then
    echo "  sudo yum install -y${MISSING}"
  elif command -v pacman >/dev/null 2>&1; then
    echo "  sudo pacman -S --needed${MISSING}"
  elif command -v zypper >/dev/null 2>&1; then
    echo "  sudo zypper install${MISSING}"
  elif command -v apk >/dev/null 2>&1; then
    echo "  apk add${MISSING}    (as root)"
  elif command -v xbps-install >/dev/null 2>&1; then
    echo "  sudo xbps-install -S${MISSING}"
  elif command -v brew >/dev/null 2>&1; then
    echo "  brew install${MISSING}"
  else
    echo "  (with your system's package manager)"
  fi
  exit 1
fi

# HTTP helpers (curl, or wget when there is no curl). HTTPS only, with retries.
http_fetch() {
  local url="$1"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --proto '=https' "${url}"
  else
    wget -qO- "${url}"
  fi
}

http_download() {
  local url="$1"
  local dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --proto '=https' "${url}" -o "${dest}"
  else
    wget -qO "${dest}" "${url}"
  fi
}

OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
  Linux)
    case "${ARCH}" in
      x86_64) ASSET_PATTERN="linux[^\"]*x86_64[^\"]*\.tar\.gz" ;;
      *)
        echo "Error: there is no prebuilt FlashAgent for Linux on ${ARCH} yet (only x86_64)."
        echo "Build it from source instead: install Rust from https://rustup.rs, then run"
        echo "  cargo install --git https://github.com/${REPO} flashagent-tui"
        echo "(or follow the build steps in https://github.com/${REPO}/blob/main/CONTRIBUTING.md)."
        exit 1
        ;;
    esac
    EXT="tar.gz"
    ;;
  Darwin)
    case "${ARCH}" in
      arm64|aarch64) ASSET_PATTERN="macos[^\"]*aarch64[^\"]*\.tar\.gz" ;;
      x86_64) ASSET_PATTERN="macos[^\"]*x86_64[^\"]*\.tar\.gz" ;;
      *) echo "Error: Unsupported macOS architecture ${ARCH}"; exit 1 ;;
    esac
    EXT="tar.gz"
    ;;
  *)
    echo "Error: Unsupported operating system ${OS}. For Windows, use PowerShell: irm https://raw.githubusercontent.com/${REPO}/main/install.ps1 | iex"
    exit 1
    ;;
esac

echo "Detected platform: ${OS} (${ARCH})"

# first_asset <link prefix> <release JSON or HTML>: the first link to this
# platform's archive. The Void Linux tarball matches the Linux pattern but is
# laid out as a package, so it is skipped.
first_asset() {
  printf '%s\n' "$2" | grep -o "$1${ASSET_PATTERN}" | grep -v -- '-void-' | head -n 1 || true
}

DOWNLOAD_URL=""

# Strategy 1: a specific version was asked for. Nothing else will do.
if [ -n "${TARGET_VERSION}" ]; then
  case "${TARGET_VERSION}" in
    [0-9]*) TARGET_VERSION="v${TARGET_VERSION}" ;;
  esac
  echo "Target version requested: ${TARGET_VERSION}"
  # The + in a stable tag (vX.Y.Z+bN) must not be read as a space.
  TAG_IN_URL="$(printf '%s' "${TARGET_VERSION}" | sed 's/+/%2B/g')"
  RELEASE_JSON=$(http_fetch "https://api.github.com/repos/${REPO}/releases/tags/${TAG_IN_URL}" 2>/dev/null || true)
  if [ -n "${RELEASE_JSON}" ]; then
    DOWNLOAD_URL=$(first_asset "https://[^\"]*" "${RELEASE_JSON}")
  fi
  # Fallback to expanded_assets if API rate-limited
  if [ -z "${DOWNLOAD_URL}" ]; then
    EXPANDED_HTML=$(http_fetch "https://github.com/${REPO}/releases/expanded_assets/${TAG_IN_URL}" 2>/dev/null || true)
    DOWNLOAD_REL=$(first_asset "/${REPO}/releases/download/[^\"]*" "${EXPANDED_HTML}")
    if [ -n "${DOWNLOAD_REL}" ]; then
      DOWNLOAD_URL="https://github.com${DOWNLOAD_REL}"
    fi
  fi
  if [ -z "${DOWNLOAD_URL}" ]; then
    echo "Error: no FlashAgent release ${TARGET_VERSION} with a ${OS} ${ARCH} build was found."
    echo "Stable releases are tagged vX.Y.Z+bN; the list is at https://github.com/${REPO}/releases"
    echo "Beta builds are not kept one by one: for the latest beta, unset FLASHAGENT_VERSION and set FLASHAGENT_CHANNEL=beta."
    exit 1
  fi
elif [ "${TARGET_CHANNEL}" != "beta" ] && [ "${TARGET_CHANNEL}" != "prerelease" ]; then
  # Strategy 2: the latest stable release (/releases/latest never returns prereleases).
  echo "Checking for latest stable release..."
  LATEST_STABLE_JSON=$(http_fetch "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null || true)
  if [ -n "${LATEST_STABLE_JSON}" ] && ! echo "${LATEST_STABLE_JSON}" | grep -q '"message":'; then
    DOWNLOAD_URL=$(first_asset "https://[^\"]*" "${LATEST_STABLE_JSON}")
    if [ -n "${DOWNLOAD_URL}" ]; then
      STABLE_TAG=$(echo "${LATEST_STABLE_JSON}" | grep -o '"tag_name": *"[^"]*"' | head -n 1 | sed -e 's/"tag_name": *"//;s/"//' || true)
      echo "✔ Found latest stable release: ${STABLE_TAG}"
    fi
  fi

  # Fallback for stable release via web redirect if API rate-limited
  if [ -z "${DOWNLOAD_URL}" ] && command -v curl >/dev/null 2>&1; then
    LATEST_REDIRECT_URL=$(curl -fsSIL --retry 3 --proto '=https' -o /dev/null -w "%{url_effective}" "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)
    if echo "${LATEST_REDIRECT_URL}" | grep -q '/releases/tag/'; then
      STABLE_TAG=$(echo "${LATEST_REDIRECT_URL}" | sed -e 's|.*/releases/tag/||')
      if [ -n "${STABLE_TAG}" ]; then
        echo "✔ Found latest stable release: ${STABLE_TAG}"
        EXPANDED_HTML=$(http_fetch "https://github.com/${REPO}/releases/expanded_assets/${STABLE_TAG}" 2>/dev/null || true)
        DOWNLOAD_REL=$(first_asset "/${REPO}/releases/download/[^\"]*" "${EXPANDED_HTML}")
        if [ -n "${DOWNLOAD_REL}" ]; then
          DOWNLOAD_URL="https://github.com${DOWNLOAD_REL}"
        fi
      fi
    fi
  fi

  # No stable release yet.
  if [ -z "${DOWNLOAD_URL}" ]; then
    echo "Notice: No official stable release published yet. Falling back to latest pre-release..."
  fi
fi

# Strategy 3: the rolling `beta` pre-release. It is edited in place for every
# beta build, so it keeps its old creation date and is NOT first in the list.
if [ -z "${DOWNLOAD_URL}" ]; then
  RELEASE_JSON=$(http_fetch "https://api.github.com/repos/${REPO}/releases/tags/beta" 2>/dev/null || true)
  if [ -n "${RELEASE_JSON}" ]; then
    DOWNLOAD_URL=$(first_asset "https://[^\"]*" "${RELEASE_JSON}")
  fi
  if [ -z "${DOWNLOAD_URL}" ]; then
    EXPANDED_HTML=$(http_fetch "https://github.com/${REPO}/releases/expanded_assets/beta" 2>/dev/null || true)
    DOWNLOAD_REL=$(first_asset "/${REPO}/releases/download/[^\"]*" "${EXPANDED_HTML}")
    if [ -n "${DOWNLOAD_REL}" ]; then
      DOWNLOAD_URL="https://github.com${DOWNLOAD_REL}"
    fi
  fi
fi

# Strategy 4: newest pre-release in the release list
if [ -z "${DOWNLOAD_URL}" ]; then
  RELEASE_JSON=$(http_fetch "https://api.github.com/repos/${REPO}/releases" 2>/dev/null || true)
  if [ -n "${RELEASE_JSON}" ]; then
    DOWNLOAD_URL=$(first_asset "https://[^\"]*" "${RELEASE_JSON}")
  fi

  # Scrape the release page when the API is rate-limited.
  if [ -z "${DOWNLOAD_URL}" ]; then
    LATEST_TAG=$(http_fetch "https://github.com/${REPO}/releases" 2>/dev/null | grep -o 'data-item-id="release-[^"]*"' | head -n 1 | sed -e 's/data-item-id="release-//;s/"//g' || true)
    if [ -n "${LATEST_TAG}" ]; then
      EXPANDED_HTML=$(http_fetch "https://github.com/${REPO}/releases/expanded_assets/${LATEST_TAG}" 2>/dev/null || true)
      DOWNLOAD_REL=$(first_asset "/${REPO}/releases/download/[^\"]*" "${EXPANDED_HTML}")
      if [ -n "${DOWNLOAD_REL}" ]; then
        DOWNLOAD_URL="https://github.com${DOWNLOAD_REL}"
      fi
    fi
  fi
fi

if [ -z "${DOWNLOAD_URL}" ]; then
  echo "Error: Could not find release asset for ${OS} ${ARCH}"
  echo "Check available releases at https://github.com/${REPO}/releases"
  exit 1
fi

TMP_DIR=$(mktemp -d)
trap 'rm -rf "${TMP_DIR}"' EXIT

echo "Downloading from ${DOWNLOAD_URL}..."
if ! http_download "${DOWNLOAD_URL}" "${TMP_DIR}/archive.${EXT}"; then
  echo "Error: the download failed. Nothing was installed."
  exit 1
fi

# 2. Check the download against the release's SHA256SUMS, as the in-app
# updater does. Releases from before the manifest existed have none: that is a
# warning. A manifest that lacks the file or disagrees with it stops the install.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print tolower($1) }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print tolower($1) }'
  else
    return 1
  fi
}

ASSET_NAME="$(printf '%s' "${DOWNLOAD_URL##*/}" | sed 's/%2[Bb]/+/g')"
SUMS_URL="${DOWNLOAD_URL%/*}/SHA256SUMS"
if ! http_download "${SUMS_URL}" "${TMP_DIR}/SHA256SUMS" 2>/dev/null; then
  echo "Warning: this release has no SHA256SUMS (older releases lack it); the download is not verified."
elif ! ACTUAL_SUM="$(sha256_of "${TMP_DIR}/archive.${EXT}")"; then
  echo "Warning: neither sha256sum nor shasum is installed; the download is not verified."
else
  # GitHub may rewrite '+' in an asset name; compare with it normalised too.
  EXPECTED_SUM="$(awk -v a="${ASSET_NAME}" '
    { n = $2; sub(/^\*/, "", n); na = a; nn = n; gsub(/\+/, ".", na); gsub(/\+/, ".", nn)
      if (n == a || nn == na) { print tolower($1); exit } }' "${TMP_DIR}/SHA256SUMS")"
  if [ -z "${EXPECTED_SUM}" ]; then
    echo "Error: ${ASSET_NAME} is not listed in the release's SHA256SUMS. Nothing was installed."
    exit 1
  fi
  if [ "${EXPECTED_SUM}" != "${ACTUAL_SUM}" ]; then
    echo "Error: checksum mismatch for ${ASSET_NAME}."
    echo "  expected ${EXPECTED_SUM}"
    echo "  got      ${ACTUAL_SUM}"
    echo "The download is damaged or was altered. Nothing was installed."
    exit 1
  fi
  echo "✔ Checksum verified (SHA256SUMS)"
fi

# 3. Find the binary in the archive before touching the existing install.
EXTRACT_DIR="${TMP_DIR}/extract"
mkdir -p "${EXTRACT_DIR}"
tar -xzf "${TMP_DIR}/archive.${EXT}" -C "${EXTRACT_DIR}"

NEW_BIN=""
if [ -f "${EXTRACT_DIR}/flashagent" ]; then
  NEW_BIN="${EXTRACT_DIR}/flashagent"
elif [ -f "${EXTRACT_DIR}/flashagent-tui" ]; then
  NEW_BIN="${EXTRACT_DIR}/flashagent-tui"
else
  NEW_BIN=$(find "${EXTRACT_DIR}" -type f \( -name "flashagent" -o -name "flashagent-tui" \) | head -n 1 || true)
fi
if [ -z "${NEW_BIN}" ] || [ ! -s "${NEW_BIN}" ]; then
  echo "Error: no flashagent binary in the downloaded archive. Nothing was installed."
  exit 1
fi

# 4. Install. The new binary is staged next to the old one, the old one is
# moved aside (a running binary can be renamed but not always overwritten),
# and it is put back if anything after that fails.
TARGET="${INSTALL_DIR}/flashagent"
BACKUP="${INSTALL_DIR}/flashagent.old"
STAGED="${INSTALL_DIR}/.flashagent-install.$$"

if ! mkdir -p "${INSTALL_DIR}" || ! cp "${NEW_BIN}" "${STAGED}" || ! chmod 755 "${STAGED}"; then
  rm -f "${STAGED}" 2>/dev/null || true
  echo "Error: cannot write to ${INSTALL_DIR}. Set INSTALL_DIR to a folder you own, or run as root."
  exit 1
fi

HAD_OLD=0
if [ -e "${TARGET}" ]; then
  rm -f "${BACKUP}" 2>/dev/null || true
  if ! mv -f "${TARGET}" "${BACKUP}"; then
    rm -f "${STAGED}"
    echo "Error: could not move the existing ${TARGET} aside. Nothing was changed."
    exit 1
  fi
  HAD_OLD=1
fi

# Takes the new binary out again and puts the previous one back, if any.
restore_old() {
  rm -f "${TARGET}" 2>/dev/null || true
  if [ "${HAD_OLD}" -eq 1 ] && [ -e "${BACKUP}" ] && mv -f "${BACKUP}" "${TARGET}"; then
    echo "The previous flashagent was put back."
  fi
}

if ! mv -f "${STAGED}" "${TARGET}"; then
  rm -f "${STAGED}" 2>/dev/null || true
  echo "Error: could not install ${TARGET}."
  restore_old
  exit 1
fi

# macOS Gatekeeper quarantine removal
if [ "${OS}" = "Darwin" ]; then
  xattr -dr com.apple.quarantine "${TARGET}" 2>/dev/null || true
fi

if ! INSTALLED_VERSION="$("${TARGET}" --version 2>/dev/null)"; then
  echo "Error: the downloaded flashagent does not run on this system (\`${TARGET} --version\` failed)."
  restore_old
  exit 1
fi

rm -f "${BACKUP}" 2>/dev/null || true
# Older installs also created a `flashagent-tui` alias; the command is just
# `flashagent` now. Only remove the alias if it is ours (a link to flashagent).
if [ -L "${INSTALL_DIR}/flashagent-tui" ] && [ "$(readlink "${INSTALL_DIR}/flashagent-tui")" = "flashagent" ]; then
  rm -f "${INSTALL_DIR}/flashagent-tui"
fi

echo "✔ Successfully installed ${INSTALLED_VERSION} to ${TARGET}"

# 5. PATH. Nothing to do when the folder is already on it; otherwise every
# detected shell gets a line.
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ON_PATH=1 ;;
  *) ON_PATH=0 ;;
esac

if [ "${ON_PATH}" -eq 1 ]; then
  echo "✔ ${INSTALL_DIR} is already on your PATH."
  echo ""
  echo "Run 'flashagent' to launch!"
  exit 0
fi

UPDATED_SHELLS=""

# 1. Fish shell support
if command -v fish >/dev/null 2>&1; then
  fish -c "fish_add_path -U '${INSTALL_DIR}'" >/dev/null 2>&1 || true
  FISH_CONFIG="${HOME}/.config/fish/config.fish"
  if [ -f "${FISH_CONFIG}" ]; then
    if ! grep -qF "${INSTALL_DIR}" "${FISH_CONFIG}"; then
      printf "\n# FlashAgent\nfish_add_path %s\n" "${INSTALL_DIR}" >> "${FISH_CONFIG}"
      UPDATED_SHELLS="${UPDATED_SHELLS} fish"
    fi
  else
    mkdir -p "$(dirname "${FISH_CONFIG}")"
    printf "# FlashAgent\nfish_add_path %s\n" "${INSTALL_DIR}" > "${FISH_CONFIG}"
    UPDATED_SHELLS="${UPDATED_SHELLS} fish"
  fi
fi

# 2. POSIX / Bash / Zsh shells
LINE_TO_ADD="export PATH=\"${INSTALL_DIR}:\$PATH\""
for RC_FILE in "${HOME}/.bashrc" "${HOME}/.bash_profile" "${HOME}/.zshrc" "${HOME}/.zprofile" "${HOME}/.profile"; do
  if [ -f "${RC_FILE}" ]; then
    if ! grep -qF "${INSTALL_DIR}" "${RC_FILE}"; then
      printf "\n# FlashAgent\n%s\n" "${LINE_TO_ADD}" >> "${RC_FILE}"
      UPDATED_SHELLS="${UPDATED_SHELLS} $(basename "${RC_FILE}")"
    fi
  fi
done

# If no POSIX rc file exists yet, create ~/.profile
if [ ! -f "${HOME}/.bashrc" ] && [ ! -f "${HOME}/.zshrc" ] && [ ! -f "${HOME}/.profile" ]; then
  printf "# FlashAgent\n%s\n" "${LINE_TO_ADD}" > "${HOME}/.profile"
  UPDATED_SHELLS="${UPDATED_SHELLS} .profile"
fi

if [ -n "${UPDATED_SHELLS}" ]; then
  echo "✔ Added ${INSTALL_DIR} to PATH in:${UPDATED_SHELLS}"
else
  echo "✔ ${INSTALL_DIR} is already added to PATH in your shell startup files."
fi

# This script ran in its own shell (curl | bash), so the terminal it was
# started from does not see the new PATH yet.
case "${SHELL:-}" in
  */zsh) RC_HINT="${HOME}/.zshrc" ;;
  */bash) RC_HINT="${HOME}/.bashrc" ;;
  *) RC_HINT="" ;;
esac
echo ""
echo "Open a new terminal, then run 'flashagent' to launch."
if [ -n "${RC_HINT}" ] && [ -f "${RC_HINT}" ] && grep -qF "${INSTALL_DIR}" "${RC_HINT}"; then
  echo "Or, in this terminal: source ${RC_HINT} && flashagent"
else
  echo "Or, in this terminal: export PATH=\"${INSTALL_DIR}:\$PATH\" && flashagent"
fi

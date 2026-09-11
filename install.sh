#!/usr/bin/env bash
set -eu
if (set -o pipefail 2>/dev/null); then
  set -o pipefail
fi

# FlashAgent one-line installer
REPO="flashback7766/FlashAgent"

# Select install directory: respect user overrides (INSTALL_DIR or BIN_DIR), otherwise system-wide for root, user-local for standard users
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

# 1. Dependency check and auto-installation
ensure_dependency() {
  local cmd="$1"
  local pkg="${2:-$1}"
  if ! command -v "${cmd}" >/dev/null 2>&1; then
    echo "Notice: Missing required tool '${cmd}'. Attempting auto-installation..."
    local SUDO=""
    if [ "$(id -u)" -ne 0 ]; then
      if command -v sudo >/dev/null 2>&1; then
        SUDO="sudo"
      elif command -v doas >/dev/null 2>&1; then
        SUDO="doas"
      fi
    fi

    if command -v pacman >/dev/null 2>&1; then
      ${SUDO} pacman -Sy --noconfirm "${pkg}"
    elif command -v apt-get >/dev/null 2>&1; then
      ${SUDO} apt-get update -y && ${SUDO} apt-get install -y "${pkg}"
    elif command -v dnf >/dev/null 2>&1; then
      ${SUDO} dnf install -y "${pkg}"
    elif command -v microdnf >/dev/null 2>&1; then
      ${SUDO} microdnf install -y "${pkg}"
    elif command -v yum >/dev/null 2>&1; then
      ${SUDO} yum install -y "${pkg}"
    elif command -v zypper >/dev/null 2>&1; then
      ${SUDO} zypper --non-interactive install "${pkg}"
    elif command -v apk >/dev/null 2>&1; then
      ${SUDO} apk add "${pkg}"
    elif command -v xbps-install >/dev/null 2>&1; then
      ${SUDO} xbps-install -Sy "${pkg}"
    elif command -v brew >/dev/null 2>&1; then
      brew install "${pkg}"
    else
      echo "Error: Required dependency '${cmd}' is missing and no supported package manager was detected."
      echo "Please install '${pkg}' manually and re-run."
      exit 1
    fi
  fi
}

# HTTP helpers (dual curl / wget support)
http_fetch() {
  local url="$1"
  if command -v curl >/dev/null 2>&1; then
    curl -sSL "${url}"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "${url}"
  else
    echo "Error: No HTTP client found (curl or wget required)." >&2
    exit 1
  fi
}

http_download() {
  local url="$1"
  local dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -sSL "${url}" -o "${dest}"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "${dest}" "${url}"
  else
    echo "Error: No HTTP client found (curl or wget required)." >&2
    exit 1
  fi
}

# Require downloader: either curl or wget
if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
  ensure_dependency curl
fi
ensure_dependency tar
ensure_dependency gzip

# Alpine Linux musl glibc compatibility layer
if [ -f /etc/alpine-release ]; then
  ensure_dependency gcompat
fi

OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
  Linux)
    case "${ARCH}" in
      x86_64) ASSET_PATTERN="linux.*x86_64.*\.tar\.gz" ;;
      *) echo "Error: Unsupported Linux architecture ${ARCH}"; exit 1 ;;
    esac
    EXT="tar.gz"
    ;;
  Darwin)
    case "${ARCH}" in
      arm64|aarch64) ASSET_PATTERN="macos.*aarch64.*\.tar\.gz" ;;
      x86_64) ASSET_PATTERN="macos.*x86_64.*\.tar\.gz" ;;
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

DOWNLOAD_URL=""

# Strategy 1: Specific version requested
if [ -n "${TARGET_VERSION}" ]; then
  echo "Target version requested: ${TARGET_VERSION}"
  # Try API first
  RELEASE_JSON=$(http_fetch "https://api.github.com/repos/${REPO}/releases/tags/${TARGET_VERSION}" 2>/dev/null || true)
  if [ -n "${RELEASE_JSON}" ]; then
    DOWNLOAD_URL=$(echo "${RELEASE_JSON}" | grep -o "https://[^\"]*${ASSET_PATTERN}" | head -n 1 || true)
  fi
  # Fallback to expanded_assets if API rate-limited
  if [ -z "${DOWNLOAD_URL}" ]; then
    EXPANDED_HTML=$(http_fetch "https://github.com/${REPO}/releases/expanded_assets/${TARGET_VERSION}" 2>/dev/null || true)
    DOWNLOAD_REL=$(echo "${EXPANDED_HTML}" | grep -o "/${REPO}/releases/download/[^\"]*${ASSET_PATTERN}" | head -n 1 || true)
    if [ -n "${DOWNLOAD_REL}" ]; then
      DOWNLOAD_URL="https://github.com${DOWNLOAD_REL}"
    fi
  fi
elif [ "${TARGET_CHANNEL}" != "beta" ] && [ "${TARGET_CHANNEL}" != "prerelease" ]; then
  # Strategy 2: Official Stable Release Priority
  # In GitHub, /releases/latest returns ONLY stable releases (never prereleases or drafts).
  echo "Checking for latest stable release..."
  LATEST_STABLE_JSON=$(http_fetch "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null || true)
  if [ -n "${LATEST_STABLE_JSON}" ] && ! echo "${LATEST_STABLE_JSON}" | grep -q '"message":'; then
    DOWNLOAD_URL=$(echo "${LATEST_STABLE_JSON}" | grep -o "https://[^\"]*${ASSET_PATTERN}" | head -n 1 || true)
    if [ -n "${DOWNLOAD_URL}" ]; then
      STABLE_TAG=$(echo "${LATEST_STABLE_JSON}" | grep -o '"tag_name": *"[^"]*"' | head -n 1 | sed -e 's/"tag_name": *"//;s/"//' || true)
      echo "✔ Found latest stable release: ${STABLE_TAG}"
    fi
  fi

  # Fallback for stable release via web redirect if API rate-limited
  if [ -z "${DOWNLOAD_URL}" ]; then
    LATEST_REDIRECT_URL=$(curl -sIL -o /dev/null -w "%{url_effective}" "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)
    if echo "${LATEST_REDIRECT_URL}" | grep -q '/releases/tag/'; then
      STABLE_TAG=$(echo "${LATEST_REDIRECT_URL}" | sed -e 's|.*/releases/tag/||')
      if [ -n "${STABLE_TAG}" ]; then
        echo "✔ Found latest stable release: ${STABLE_TAG}"
        EXPANDED_HTML=$(http_fetch "https://github.com/${REPO}/releases/expanded_assets/${STABLE_TAG}" 2>/dev/null || true)
        DOWNLOAD_REL=$(echo "${EXPANDED_HTML}" | grep -o "/${REPO}/releases/download/[^\"]*${ASSET_PATTERN}" | head -n 1 || true)
        if [ -n "${DOWNLOAD_REL}" ]; then
          DOWNLOAD_URL="https://github.com${DOWNLOAD_REL}"
        fi
      fi
    fi
  fi

  # If no stable release exists yet in the repository (e.g. project is still in beta pre-release phase)
  if [ -z "${DOWNLOAD_URL}" ]; then
    echo "Notice: No official stable release published yet. Falling back to latest pre-release..."
  fi
fi

# Strategy 3: the rolling `beta` pre-release. It is edited in place for every
# beta build, so it keeps its old creation date and is NOT first in the list.
if [ -z "${DOWNLOAD_URL}" ]; then
  RELEASE_JSON=$(http_fetch "https://api.github.com/repos/${REPO}/releases/tags/beta" 2>/dev/null || true)
  if [ -n "${RELEASE_JSON}" ]; then
    DOWNLOAD_URL=$(echo "${RELEASE_JSON}" | grep -o "https://[^\"]*${ASSET_PATTERN}" | head -n 1 || true)
  fi
  if [ -z "${DOWNLOAD_URL}" ]; then
    EXPANDED_HTML=$(http_fetch "https://github.com/${REPO}/releases/expanded_assets/beta" 2>/dev/null || true)
    DOWNLOAD_REL=$(echo "${EXPANDED_HTML}" | grep -o "/${REPO}/releases/download/[^\"]*${ASSET_PATTERN}" | head -n 1 || true)
    if [ -n "${DOWNLOAD_REL}" ]; then
      DOWNLOAD_URL="https://github.com${DOWNLOAD_REL}"
    fi
  fi
fi

# Strategy 4: newest pre-release in the release list
if [ -z "${DOWNLOAD_URL}" ]; then
  RELEASE_JSON=$(http_fetch "https://api.github.com/repos/${REPO}/releases" 2>/dev/null || true)
  if [ -n "${RELEASE_JSON}" ]; then
    DOWNLOAD_URL=$(echo "${RELEASE_JSON}" | grep -o "https://[^\"]*${ASSET_PATTERN}" | head -n 1 || true)
    if [ -z "${DOWNLOAD_URL}" ]; then
      DOWNLOAD_URL=$(echo "${RELEASE_JSON}" | grep -o "https://[^\"]*flashagent[^\"]*${ARCH}[^\"]*\.${EXT}" | head -n 1 || true)
    fi
  fi

  # GitHub web scraping fallback (immune to GitHub API rate limits)
  if [ -z "${DOWNLOAD_URL}" ]; then
    LATEST_TAG=$(http_fetch "https://github.com/${REPO}/releases" 2>/dev/null | grep -o 'data-item-id="release-[^"]*"' | head -n 1 | sed -e 's/data-item-id="release-//;s/"//g' || true)
    if [ -n "${LATEST_TAG}" ]; then
      EXPANDED_HTML=$(http_fetch "https://github.com/${REPO}/releases/expanded_assets/${LATEST_TAG}" 2>/dev/null || true)
      DOWNLOAD_REL=$(echo "${EXPANDED_HTML}" | grep -o "/${REPO}/releases/download/[^\"]*${ASSET_PATTERN}" | head -n 1 || true)
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
http_download "${DOWNLOAD_URL}" "${TMP_DIR}/archive.${EXT}"

mkdir -p "${INSTALL_DIR}"
tar -xzf "${TMP_DIR}/archive.${EXT}" -C "${TMP_DIR}"

# Atomic swap: move existing binary out of the way if currently running
if [ -f "${INSTALL_DIR}/flashagent" ]; then
  rm -f "${INSTALL_DIR}/flashagent.old" 2>/dev/null || true
  mv -f "${INSTALL_DIR}/flashagent" "${INSTALL_DIR}/flashagent.old" 2>/dev/null || true
fi

# Locate binary in extracted archive
if [ -f "${TMP_DIR}/flashagent" ]; then
  mv -f "${TMP_DIR}/flashagent" "${INSTALL_DIR}/flashagent"
elif [ -f "${TMP_DIR}/flashagent-tui" ]; then
  mv -f "${TMP_DIR}/flashagent-tui" "${INSTALL_DIR}/flashagent"
else
  FOUND_BIN=$(find "${TMP_DIR}" -type f \( -name "flashagent" -o -name "flashagent-tui" \) | head -n 1 || true)
  if [ -z "${FOUND_BIN}" ]; then
    FOUND_BIN=$(find "${TMP_DIR}" -type f -perm -111 2>/dev/null | head -n 1 || true)
  fi
  if [ -n "${FOUND_BIN}" ]; then
    mv -f "${FOUND_BIN}" "${INSTALL_DIR}/flashagent"
  else
    echo "Error: Binary not found in extracted archive"
    exit 1
  fi
fi

chmod +x "${INSTALL_DIR}/flashagent"
ln -sf flashagent "${INSTALL_DIR}/flashagent-tui" 2>/dev/null || true
rm -f "${INSTALL_DIR}/flashagent.old" 2>/dev/null || true

# macOS Gatekeeper quarantine removal
if [ "${OS}" = "Darwin" ]; then
  xattr -dr com.apple.quarantine "${INSTALL_DIR}/flashagent" 2>/dev/null || true
fi

echo "✔ Successfully installed FlashAgent to ${INSTALL_DIR}/flashagent"

# Auto-add INSTALL_DIR to PATH across all detected shell environments
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

export PATH="${INSTALL_DIR}:${PATH}"

if [ -n "${UPDATED_SHELLS}" ]; then
  echo "✔ Automatically added ${INSTALL_DIR} to PATH in:${UPDATED_SHELLS}"
else
  echo "✔ ${INSTALL_DIR} is already in your shell PATH."
fi

echo ""
echo "Run 'flashagent' to launch!"

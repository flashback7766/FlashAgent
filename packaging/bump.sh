#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Usage:
#   ./packaging/bump.sh mini             # +1
#   ./packaging/bump.sh small-feature    # +3
#   ./packaging/bump.sh medium           # +5
#   ./packaging/bump.sh big              # +10
#   ./packaging/bump.sh major            # +15
#   ./packaging/bump.sh big small-feature mini  # sums to +14 (capped at +15)
#   ./packaging/bump.sh <number>         # explicit custom increment (capped at +15)

# Stable release: ./packaging/bump.sh stable 1.0.0
# Tags vX.Y.Z+bN, where bN is the newest beta build: the stable release is
# that build, released, so the updater can tell whether a later beta is newer.
# CHANGELOG.md needs a '## vX.Y.Z+bN — title' section first, as a beta build
# needs '## bN — title'.
if [ "${1:-}" = "stable" ]; then
    SEMVER="${2:-}"
    if ! [[ "${SEMVER}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "Usage: $0 stable <MAJOR.MINOR.PATCH>   e.g. $0 stable 1.0.0" >&2
        exit 1
    fi
    BUILD_TAG="$(git -C "${ROOT_DIR}" describe --tags --match 'b[0-9]*' --abbrev=0 2>/dev/null || true)"
    if ! [[ "${BUILD_TAG}" =~ ^b[0-9]+$ ]]; then
        echo "No beta build tag (bN) found to cut the stable release from." >&2
        exit 1
    fi
    if git -C "${ROOT_DIR}" tag --list "v${SEMVER}+b*" | grep -q .; then
        echo "v${SEMVER} is already released: $(git -C "${ROOT_DIR}" tag --list "v${SEMVER}+b*")" >&2
        exit 1
    fi
    NEW_TAG="v${SEMVER}+${BUILD_TAG}"
    echo "=========================================="
    echo " FlashAgent Stable Release"
    echo " Cut from build : ${BUILD_TAG}"
    echo " New release   : ${NEW_TAG}"
    echo "=========================================="
    # Same checks as a beta build: the release workflow takes the notes of the
    # permanent release from this section, and the what's-new screen needs it.
    if git -C "${ROOT_DIR}" rev-parse -q --verify "refs/tags/${NEW_TAG}" >/dev/null; then
        echo "Tag ${NEW_TAG} already exists. A published tag is never moved." >&2
        exit 1
    fi
    if ! awk -v h="## ${NEW_TAG} " 'index($0, h) == 1 { found = 1 } END { exit !found }' "${ROOT_DIR}/CHANGELOG.md"; then
        echo "CHANGELOG.md has no '## ${NEW_TAG} — ...' section. Write it first; the release notes are taken from it." >&2
        exit 1
    fi
    OTHER_CHANGES="$(git -C "${ROOT_DIR}" status --porcelain --untracked-files=no | grep -v ' CHANGELOG.md$' || true)"
    if [ -n "${OTHER_CHANGES}" ]; then
        echo "Uncommitted changes besides CHANGELOG.md; commit them first so the release commit is only the release:" >&2
        echo "${OTHER_CHANGES}" >&2
        exit 1
    fi
    if [ -f "${ROOT_DIR}/README.md" ]; then
        sed -i "s/> \*\*Status:\*\* .*/> **Status:** Stable v${SEMVER}/" "${ROOT_DIR}/README.md"
    fi
    SUMMARY="$(grep -m1 -F "## ${NEW_TAG} " "${ROOT_DIR}/CHANGELOG.md" | sed "s/^## ${NEW_TAG} — //")"
    git -C "${ROOT_DIR}" add CHANGELOG.md README.md
    # Nothing to commit when the section was committed earlier and the status
    # line already says this release.
    if ! git -C "${ROOT_DIR}" diff --cached --quiet; then
        git -C "${ROOT_DIR}" commit -q -m "Release ${NEW_TAG}: ${SUMMARY}"
    fi
    git -C "${ROOT_DIR}" tag -a "${NEW_TAG}" -m "FlashAgent ${NEW_TAG}"
    echo "Committed and tagged ${NEW_TAG}."
    echo "Publish: git push origin HEAD '${NEW_TAG}'   (the release workflow runs the tests before building)"
    exit 0
fi

if [ "$#" -eq 0 ]; then
    echo "Usage: $0 <mini|small-feature|medium|big|major|N> ...   (beta build)"
    echo "       $0 stable <MAJOR.MINOR.PATCH>                    (stable release)"
    echo "  mini          : +1 (typo, padding, small formatting error)"
    echo "  small-feature : +3 (handy shortcut, indicator, small feature)"
    echo "  medium        : +5 (medium bugfix, input, session restoration)"
    echo "  big           : +10 (big bugfix, IPC/networking, updater transition)"
    echo "  major         : +15 (huge functionality, whole track/milestone)"
    echo "  N             : +N exactly, for a release the types do not measure"
    exit 1
fi

TOTAL_INC=0
# A number given outright is taken as it is; only the named types are capped.
EXPLICIT_INC=0

for arg in "$@"; do
    case "${arg,,}" in
        mini|fix|micro)
            INC=1
            ;;
        small-feature|feature|feat|small)
            INC=3
            ;;
        medium|med)
            INC=5
            ;;
        big|large)
            INC=10
            ;;
        major|huge|milestone)
            INC=15
            ;;
        *)
            if [[ "${arg}" =~ ^[0-9]+$ ]]; then
                EXPLICIT_INC=$(( EXPLICIT_INC + arg ))
                continue
            else
                echo "Unknown change type: ${arg}" >&2
                exit 1
            fi
            ;;
    esac
    TOTAL_INC=$(( TOTAL_INC + INC ))
done

# The named types add up to at most +15 per release; a number is not capped.
if [ "${TOTAL_INC}" -gt 15 ]; then
    echo "Notice: Capping the named increments from +${TOTAL_INC} to +15; give a number to jump further."
    TOTAL_INC=15
fi
TOTAL_INC=$(( TOTAL_INC + EXPLICIT_INC ))

CURRENT_TAG="$(git -C "${ROOT_DIR}" describe --tags --match 'b[0-9]*' --exact-match 2>/dev/null || git -C "${ROOT_DIR}" describe --tags --match 'b[0-9]*' 2>/dev/null || echo "b286")"
CURRENT_NUM="${CURRENT_TAG#b}"
CURRENT_NUM="${CURRENT_NUM%%-*}"

if ! [[ "${CURRENT_NUM}" =~ ^[0-9]+$ ]]; then
    CURRENT_NUM=274
fi

NEW_NUM=$(( CURRENT_NUM + TOTAL_INC ))
NEW_TAG="b${NEW_NUM}"

echo "=========================================="
echo " FlashAgent Beta Build Bump"
echo " Current build : ${CURRENT_TAG} (${CURRENT_NUM})"
echo " Increment     : +${TOTAL_INC}"
echo " New build     : ${NEW_TAG}"
echo "=========================================="

# The tag goes on the release commit itself; tagging first put it on the
# commit before, and the binaries did not know their version.
if git -C "${ROOT_DIR}" rev-parse -q --verify "refs/tags/${NEW_TAG}" >/dev/null; then
    echo "Tag ${NEW_TAG} already exists. A published tag is never moved; pick the next number." >&2
    exit 1
fi
if ! grep -q "^## ${NEW_TAG} " "${ROOT_DIR}/CHANGELOG.md"; then
    echo "CHANGELOG.md has no '## ${NEW_TAG} — ...' section. Write it first; the release notes link to it." >&2
    exit 1
fi
OTHER_CHANGES="$(git -C "${ROOT_DIR}" status --porcelain --untracked-files=no | grep -v ' CHANGELOG.md$' || true)"
if [ -n "${OTHER_CHANGES}" ]; then
    echo "Uncommitted changes besides CHANGELOG.md; commit them first so the release commit is only the release:" >&2
    echo "${OTHER_CHANGES}" >&2
    exit 1
fi

if [ -f "${ROOT_DIR}/packaging/arch/PKGBUILD" ]; then
    sed -i "s/^pkgver=.*/pkgver=${NEW_TAG}/" "${ROOT_DIR}/packaging/arch/PKGBUILD"
fi
if [ -f "${ROOT_DIR}/README.md" ]; then
    sed -i "s/> \*\*Status:\*\* Beta b[0-9]\+/> **Status:** Beta ${NEW_TAG}/" "${ROOT_DIR}/README.md"
fi

SUMMARY="$(grep -m1 "^## ${NEW_TAG} " "${ROOT_DIR}/CHANGELOG.md" | sed "s/^## ${NEW_TAG} — //")"
git -C "${ROOT_DIR}" add CHANGELOG.md README.md packaging/arch/PKGBUILD
git -C "${ROOT_DIR}" commit -q -m "Release ${NEW_TAG}: ${SUMMARY}"
git -C "${ROOT_DIR}" tag -a "${NEW_TAG}" -m "FlashAgent ${NEW_TAG} (+${TOTAL_INC})"
echo "Committed and tagged ${NEW_TAG}."
echo "Publish: git push origin HEAD '${NEW_TAG}'   (the release workflow runs the tests before building)"

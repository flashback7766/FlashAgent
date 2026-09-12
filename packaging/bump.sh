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

if [ "$#" -eq 0 ]; then
    echo "Usage: $0 <mini|small-feature|medium|big|major|N> ..."
    echo "  mini          : +1 (typo, padding, small formatting error)"
    echo "  small-feature : +3 (handy shortcut, indicator, small feature)"
    echo "  medium        : +5 (medium bugfix, input, session restoration)"
    echo "  big           : +10 (big bugfix, IPC/networking, updater transition)"
    echo "  major         : +15 (huge functionality, whole track/milestone)"
    exit 1
fi

TOTAL_INC=0

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
                INC="${arg}"
            else
                echo "Unknown change type: ${arg}" >&2
                exit 1
            fi
            ;;
    esac
    TOTAL_INC=$(( TOTAL_INC + INC ))
done

# Hard cap at +15 per release
if [ "${TOTAL_INC}" -gt 15 ]; then
    echo "Notice: Capping total increment from +${TOTAL_INC} to maximum allowed +15."
    TOTAL_INC=15
fi

# Find current build tag
CURRENT_TAG="$(git -C "${ROOT_DIR}" describe --tags --match 'b[0-9]*' --exact-match 2>/dev/null || git -C "${ROOT_DIR}" describe --tags --match 'b[0-9]*' 2>/dev/null || echo "b262")"
CURRENT_NUM="${CURRENT_TAG#b}"
CURRENT_NUM="${CURRENT_NUM%%-*}"

if ! [[ "${CURRENT_NUM}" =~ ^[0-9]+$ ]]; then
    CURRENT_NUM=262
fi

NEW_NUM=$(( CURRENT_NUM + TOTAL_INC ))
NEW_TAG="b${NEW_NUM}"

echo "=========================================="
echo " FlashAgent Beta Build Bump"
echo " Current build : ${CURRENT_TAG} (${CURRENT_NUM})"
echo " Increment     : +${TOTAL_INC}"
echo " New build     : ${NEW_TAG}"
echo "=========================================="

# Update PKGBUILD and README.md
if [ -f "${ROOT_DIR}/packaging/arch/PKGBUILD" ]; then
    sed -i "s/^pkgver=.*/pkgver=${NEW_TAG}/" "${ROOT_DIR}/packaging/arch/PKGBUILD"
fi
if [ -f "${ROOT_DIR}/README.md" ]; then
    sed -i "s/> \*\*Status:\*\* Beta b[0-9]\+/> **Status:** Beta ${NEW_TAG}/" "${ROOT_DIR}/README.md"
fi

# Create annotated tag
git -C "${ROOT_DIR}" tag -fa "${NEW_TAG}" -m "FlashAgent ${NEW_TAG} (+${TOTAL_INC})"
echo "Tag ${NEW_TAG} created successfully."
echo "To package, run: ./packaging/build-all.sh ${NEW_TAG}"

#!/usr/bin/env bash
# FlashAgent uninstaller for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/flashback7766/FlashAgent/main/uninstall.sh | bash
#
# When a working `flashagent` is on this machine it does the job itself
# (`flashagent --uninstall`): it knows about package installs and asks about
# your data part by part. This script only does the removal by hand when that
# binary is missing or broken.
set -euo pipefail

# Questions need the terminal even when this script arrives on a pipe.
TTY=/dev/tty
if [ ! -r "$TTY" ]; then
  TTY=/dev/stdin
fi

ask() {
  # ask "question" default(y|n)
  local answer hint
  if [ "$2" = "y" ]; then hint="[Y/n]"; else hint="[y/N]"; fi
  printf "%s %s " "$1" "$hint"
  read -r answer <"$TTY" || answer=""
  answer="$(printf '%s' "$answer" | tr '[:upper:]' '[:lower:]')"
  if [ -z "$answer" ]; then answer="$2"; fi
  [ "$answer" = "y" ] || [ "$answer" = "yes" ]
}

for candidate in "$(command -v flashagent 2>/dev/null || true)" "${HOME}/.local/bin/flashagent" "/usr/local/bin/flashagent"; do
  if [ -n "$candidate" ] && [ -x "$candidate" ] && "$candidate" --version >/dev/null 2>&1; then
    exec "$candidate" --uninstall <"$TTY"
  fi
done

echo "No working flashagent binary found; removing what the installer put down."
echo ""

removed=""
for dir in "${HOME}/.local/bin" "/usr/local/bin"; do
  for name in flashagent flashagent-tui flashagent.old; do
    target="${dir}/${name}"
    if [ -e "$target" ] || [ -L "$target" ]; then
      if rm -f "$target" 2>/dev/null; then
        removed="${removed}\n  removed  ${target}"
      else
        echo "  could not remove ${target} (try: sudo rm ${target})"
      fi
    fi
  done
  # The installer's PATH line goes only when the folder is left empty.
  if [ -d "$dir" ] && [ -z "$(ls -A "$dir" 2>/dev/null)" ]; then
    for rc in .bashrc .bash_profile .zshrc .zprofile .profile .config/fish/config.fish; do
      file="${HOME}/${rc}"
      [ -f "$file" ] || continue
      grep -qxF "# FlashAgent" "$file" || continue
      cp "$file" "${file}.flashagent-uninstall.bak"
      awk -v dir="$dir" '
        { lines[NR] = $0 }
        END {
          for (i = 1; i <= NR; i++) {
            nxt = lines[i + 1]
            if (lines[i] == "# FlashAgent" && index(nxt, dir) > 0 && (nxt ~ /^export PATH=/ || nxt ~ /^fish_add_path/)) {
              if (out > 0 && kept[out] == "") { out-- }
              i++
              continue
            }
            kept[++out] = lines[i]
          }
          for (i = 1; i <= out; i++) print kept[i]
        }' "${file}.flashagent-uninstall.bak" >"$file"
      removed="${removed}\n  removed  PATH line in ${file} (backup: ${file}.flashagent-uninstall.bak)"
    done
  fi
done

data="${HOME}/.flashagent"
if [ -d "$data" ]; then
  echo "Your data is in ${data}: settings, saved sessions, memory, /rewind copies."
  if ask "Delete it too?" n; then
    rm -rf "$data"
    removed="${removed}\n  removed  ${data}"
  else
    echo "  kept     ${data}"
  fi
fi

printf "\nFlashAgent is uninstalled.%b\n" "$removed"

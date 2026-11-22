#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${BOT_SECRETS_PASSPHRASE:-}" && -n "${BOT_SECRETS_PASSPHRASE_FILE:-}" ]]; then
  if [[ ! -r "${BOT_SECRETS_PASSPHRASE_FILE}" ]]; then
    echo "bot: error: BOT_SECRETS_PASSPHRASE_FILE '${BOT_SECRETS_PASSPHRASE_FILE}' is not readable" >&2
    exit 1
  fi
  pass="$(<"${BOT_SECRETS_PASSPHRASE_FILE}")"
  pass="${pass%$'\n'}"
  pass="${pass%$'\r'}"
  export BOT_SECRETS_PASSPHRASE="${pass}"
fi

if command -v Xvfb >/dev/null 2>&1 && [[ "${PRAXI_DISABLE_DESKTOP:-0}" != "1" ]]; then
  export DISPLAY="${DISPLAY:-:99}"
  export XAUTHORITY="${XAUTHORITY:-${BOT_DATA_DIR:-/var/lib/praxi}/.Xauthority}"
  export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=${BOT_DATA_DIR:-/var/lib/praxi}/.dbus-session}"
  export XDG_SESSION_TYPE="${XDG_SESSION_TYPE:-x11}"
  export NO_AT_BRIDGE="${NO_AT_BRIDGE:-0}"
  export GTK_MODULES="${GTK_MODULES:-gail:atk-bridge}"
  export QT_ACCESSIBILITY="${QT_ACCESSIBILITY:-1}"
  export QT_LINUX_ACCESSIBILITY_ALWAYS_ON="${QT_LINUX_ACCESSIBILITY_ALWAYS_ON:-1}"
  export GNOME_ACCESSIBILITY="${GNOME_ACCESSIBILITY:-1}"
  if [[ -z "${BOT_BROWSER_PATH:-}" && -x /opt/praxi-browser/chrome ]]; then
    export BOT_BROWSER_PATH="/opt/praxi-browser/chrome"
  fi
fi

exec /opt/praxi/bin/bot "$@"

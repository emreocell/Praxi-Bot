#!/usr/bin/env bash
# Provision a headless Debian/Ubuntu node (amd64 or arm64, e.g. Oracle ARM VM)
# with Xvfb (:99), Openbox, D-Bus + AT-SPI2 accessibility, Chromium, and an
# optional localhost-only x11vnc service.
#
# Usage:
#   ./scripts/linux/setup-headless.sh [options]
#
# Options:
#   --vnc, --enable-vnc      Install and enable x11vnc bound strictly to localhost
#   --no-vnc                 Disable x11vnc service if previously configured (default)
#   --vnc-port <port>        Localhost TCP port for x11vnc (default: 5900)
#   --display <:N>           X11 display number for Xvfb (default: :99)
#   --screen <WxHxD>         Xvfb screen geometry (default: 1920x1080x24)
#   --skip-packages          Skip apt-get package installation
#   -h, --help               Show this help message

set -euo pipefail

ENABLE_VNC=0
VNC_PORT="5900"
DISPLAY_NUM=":99"
SCREEN_SPEC="1920x1080x24"
SKIP_PACKAGES=0

usage() {
  sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --vnc|--enable-vnc)
      ENABLE_VNC=1
      shift
      ;;
    --no-vnc)
      ENABLE_VNC=0
      shift
      ;;
    --vnc-port)
      VNC_PORT="${2:?--vnc-port requires a port number}"
      shift 2
      ;;
    --display)
      DISPLAY_NUM="${2:?--display requires a display value such as :99}"
      shift 2
      ;;
    --screen)
      SCREEN_SPEC="${2:?--screen requires WxHxD such as 1920x1080x24}"
      shift 2
      ;;
    --skip-packages)
      SKIP_PACKAGES=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument '$1'" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: setup-headless.sh must be run on a Linux host (Debian/Ubuntu)." >&2
  exit 1
fi

ARCH="$(dpkg --print-architecture 2>/dev/null || uname -m)"
case "${ARCH}" in
  amd64|x86_64|arm64|aarch64)
    echo "[praxi-headless] target architecture: ${ARCH}"
    ;;
  *)
    echo "[praxi-headless] warning: untested architecture '${ARCH}', continuing..." >&2
    ;;
esac

if [[ "${SKIP_PACKAGES}" -eq 0 ]]; then
  if ! command -v apt-get >/dev/null 2>&1; then
    echo "error: apt-get not found; this script supports Debian and Ubuntu." >&2
    exit 1
  fi

  SUDO=""
  if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
    SUDO="sudo"
  fi

  echo "[praxi-headless] updating apt package index..."
  ${SUDO} apt-get update -y

  PACKAGES=(
    xvfb
    openbox
    dbus
    dbus-x11
    dbus-user-session
    at-spi2-core
    libatk-adaptor
    libgail-common
    libglib2.0-bin
    x11-utils
    x11-xserver-utils
    xdg-utils
    fonts-liberation
    fonts-noto-core
  )

  apt_candidate() {
    local candidate
    candidate="$(apt-cache policy "$1" | awk '/Candidate:/ {print $2}')"
    [[ -n "${candidate}" && "${candidate}" != "(none)" && "${candidate}" != *snap* ]]
  }

  # Only Debian's native package is accepted; Ubuntu's chromium/chromium-browser
  # resolve to snap wrappers, which are unsupported under systemd hardening.
  distro_id="$(. /etc/os-release 2>/dev/null; printf '%s' "${ID:-}")"
  if [[ "${distro_id}" == "debian" ]] && apt_candidate chromium; then
    PACKAGES+=(chromium)
  elif [[ -x /opt/praxi-browser/chrome ]]; then
    echo "[praxi-headless] using existing non-snap browser at /opt/praxi-browser/chrome"
  else
    echo "[praxi-headless] warning: no non-snap apt chromium package found; install Chrome for Testing via scripts/install-linux.sh --with-desktop (see docs/guide/remote-host.md)" >&2
  fi

  if [[ "${ENABLE_VNC}" -eq 1 ]]; then
    PACKAGES+=(x11vnc)
  fi

  echo "[praxi-headless] installing packages: ${PACKAGES[*]}"
  DEBIAN_FRONTEND=noninteractive ${SUDO} apt-get install -y --no-install-recommends "${PACKAGES[@]}"
fi

BIN_DIR="${HOME}/.local/bin"
SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
SESSION_SCRIPT="${BIN_DIR}/praxi-headless-session.sh"
DESKTOP_UNIT="${SYSTEMD_USER_DIR}/praxi-headless-desktop.service"
VNC_UNIT="${SYSTEMD_USER_DIR}/praxi-headless-vnc.service"

mkdir -p "${BIN_DIR}" "${SYSTEMD_USER_DIR}"

cat > "${SESSION_SCRIPT}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

export DISPLAY="${DISPLAY_NUM}"
export XDG_SESSION_TYPE="x11"
export NO_AT_BRIDGE=0
export GTK_MODULES="gail:atk-bridge"
export QT_ACCESSIBILITY=1
export QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
export GNOME_ACCESSIBILITY=1

XVFB_PID=""
WM_PID=""

cleanup() {
  if [[ -n "\${WM_PID}" ]] && kill -0 "\${WM_PID}" 2>/dev/null; then
    kill "\${WM_PID}" 2>/dev/null || true
  fi
  if [[ -n "\${XVFB_PID}" ]] && kill -0 "\${XVFB_PID}" 2>/dev/null; then
    kill "\${XVFB_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

Xvfb "\${DISPLAY}" -screen 0 "${SCREEN_SPEC}" -ac -nolisten tcp +extension RANDR +extension XTEST &
XVFB_PID=\$!

for _ in \$(seq 1 50); do
  if xdpyinfo -display "\${DISPLAY}" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

if command -v dbus-update-activation-environment >/dev/null 2>&1; then
  dbus-update-activation-environment --systemd \
    DISPLAY XDG_SESSION_TYPE NO_AT_BRIDGE GTK_MODULES \
    QT_ACCESSIBILITY QT_LINUX_ACCESSIBILITY_ALWAYS_ON GNOME_ACCESSIBILITY || true
fi

if command -v gsettings >/dev/null 2>&1; then
  gsettings set org.gnome.desktop.interface toolkit-accessibility true 2>/dev/null || true
fi

if command -v gdbus >/dev/null 2>&1; then
  gdbus call --session \
    --dest org.a11y.Bus \
    --object-path /org/a11y/bus \
    --method org.freedesktop.DBus.Properties.Set \
    org.a11y.Status IsEnabled "<true>" >/dev/null 2>&1 || true
fi

openbox &
WM_PID=\$!

wait -n "\${XVFB_PID}" "\${WM_PID}"
EOF

chmod +x "${SESSION_SCRIPT}"

cat > "${DESKTOP_UNIT}" <<EOF
[Unit]
Description=Praxi Headless X11 Desktop (Xvfb ${DISPLAY_NUM} + Openbox + AT-SPI2)
After=dbus.socket

[Service]
Type=simple
Environment="DISPLAY=${DISPLAY_NUM}"
Environment="XDG_SESSION_TYPE=x11"
Environment="NO_AT_BRIDGE=0"
Environment="GTK_MODULES=gail:atk-bridge"
Environment="QT_ACCESSIBILITY=1"
Environment="QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1"
Environment="GNOME_ACCESSIBILITY=1"
ExecStart=${SESSION_SCRIPT}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
EOF

if [[ "${ENABLE_VNC}" -eq 1 ]]; then
  cat > "${VNC_UNIT}" <<EOF
[Unit]
Description=Praxi Localhost-Only VNC Server (x11vnc on ${DISPLAY_NUM})
After=praxi-headless-desktop.service
Requires=praxi-headless-desktop.service

[Service]
Type=simple
Environment="DISPLAY=${DISPLAY_NUM}"
ExecStart=/usr/bin/x11vnc -display ${DISPLAY_NUM} -localhost -rfbport ${VNC_PORT} -forever -shared -noxdamage
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
EOF
else
  if [[ -f "${VNC_UNIT}" ]]; then
    systemctl --user disable --now praxi-headless-vnc.service 2>/dev/null || true
    rm -f "${VNC_UNIT}"
  fi
fi

if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
  systemctl --user daemon-reload
  systemctl --user enable --now praxi-headless-desktop.service
  if [[ "${ENABLE_VNC}" -eq 1 ]]; then
    systemctl --user enable --now praxi-headless-vnc.service
  fi
  echo "[praxi-headless] systemd user services enabled and started."
else
  echo "[praxi-headless] systemd user session is not active in this shell."
  echo "  Run 'loginctl enable-linger \$USER' and then:"
  echo "  systemctl --user daemon-reload && systemctl --user enable --now praxi-headless-desktop.service"
fi

echo "[praxi-headless] setup complete:"
echo "  Session script : ${SESSION_SCRIPT}"
echo "  Desktop unit   : ${DESKTOP_UNIT}"
if [[ "${ENABLE_VNC}" -eq 1 ]]; then
  echo "  VNC unit       : ${VNC_UNIT} (bound strictly to 127.0.0.1:${VNC_PORT})"
  echo "  SSH tunnel     : ssh -L ${VNC_PORT}:127.0.0.1:${VNC_PORT} \$(whoami)@<host>"
fi

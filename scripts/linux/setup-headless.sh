#!/usr/bin/env bash
# Provision a headless Debian/Ubuntu (or Fedora/RHEL with --system) node
# (x86_64 or aarch64) with Xvfb, minimal Xfce desktop session, D-Bus + AT-SPI2
# accessibility, private Xauthority, and non-snap Chromium verification.
#
# Reconciled with scripts/install-linux.sh:
# - In --system mode (root), configures the shared system units for user 'praxi'
#   (praxi.slice, praxi-xvfb.service, praxi-desktop.service, drop-ins, env files)
#   and cleans up any conflicting user-level praxi-headless-desktop unit.
# - In --user mode (default when non-system), configures a user-level minimal
#   Xfce session on a non-conflicting display (refusing displays owned by
#   praxi-xvfb.service).
#
# Usage:
#   ./scripts/linux/setup-headless.sh [options]
#
# Options:
#   --system                   Configure system-wide praxi desktop units (requires root)
#   --user                     Configure user-level dev session (default)
#   --session <xfce|none>      Desktop session type (default: xfce; 'none' removes session in --system mode)
#   --with-session-apps        Also install/expose xfce4-terminal and thunar (runs as praxi with read access to praxi.env)
#   --without-session-apps     Hide xfce4-terminal and thunar menu entries (does not uninstall packages)
#   --without-desktop-session  Alias for --session none (--system mode)
#   --without-desktop          Remove praxi-desktop.service and praxi-xvfb.service (--system mode)
#   --display <:N>             X11 display number for Xvfb (default: :99)
#   --screen <WxHxD>           Xvfb screen geometry (default: 1920x1080x24)
#   --skip-packages            Skip apt/dnf package installation
#   --no-start                 Write units/config and enable without starting services
#   --dry-run                  Print planned actions without modifying the host
#   --no-vnc                   Disable and remove any legacy praxi-headless-vnc.service
#   -h, --help                 Show this help message

set -euo pipefail

MODE="user"
SESSION_MODE="xfce"
WITHOUT_DESKTOP=0
WITH_SESSION_APPS=0
WITHOUT_SESSION_APPS=0
KEEP_SESSION_APPS=0
SESSION_APPS_ACTIVE=0
DISPLAY_NUM=":99"
SCREEN_SPEC="1920x1080x24"
SKIP_PACKAGES=0
NO_START=0
DRY_RUN=0

PREFIX="/opt/praxi"
LIBEXEC_DIR="${PREFIX}/libexec"
DATA_DIR="/var/lib/praxi"
CONF_DIR="/etc/praxi"
ENV_FILE="${CONF_DIR}/praxi.env"
NODE_ENV_FILE="${CONF_DIR}/praxi-node.env"
UNIT_DIR="/etc/systemd/system"
SLICE_NAME="praxi.slice"
SVC_USER="praxi"
XAUTH_FILE="${DATA_DIR}/.Xauthority"
DBUS_SOCKET="${DATA_DIR}/.dbus-session"

log() { printf '[praxi-headless] %s\n' "$*"; }
warn() { printf '[praxi-headless] warning: %s\n' "$*" >&2; }
die() { printf '[praxi-headless] error: %s\n' "$*" >&2; exit 1; }
need_arg() { [[ $# -ge 2 && -n "$2" ]] || die "$1 requires a value"; }

usage() {
  sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --system)
      MODE="system"
      shift
      ;;
    --user)
      MODE="user"
      shift
      ;;
    --session)
      need_arg "$@"
      case "$2" in
        xfce|none) SESSION_MODE="$2" ;;
        *) die "invalid --session '$2' (expected xfce or none)" ;;
      esac
      shift 2
      ;;
    --without-desktop-session)
      SESSION_MODE="none"
      shift
      ;;
    --with-session-apps)
      WITH_SESSION_APPS=1
      shift
      ;;
    --without-session-apps)
      WITHOUT_SESSION_APPS=1
      shift
      ;;
    --without-desktop)
      WITHOUT_DESKTOP=1
      SESSION_MODE="none"
      shift
      ;;
    --vnc|--enable-vnc|--vnc-port)
      die "VNC/RDP listeners are disabled by security policy; use Praxi live-view without VNC"
      ;;
    --no-vnc)
      shift
      ;;
    --display)
      need_arg "$@"
      DISPLAY_NUM="$2"
      shift 2
      ;;
    --screen)
      need_arg "$@"
      SCREEN_SPEC="$2"
      shift 2
      ;;
    --skip-packages)
      SKIP_PACKAGES=1
      shift
      ;;
    --no-start)
      NO_START=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
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

[[ "${DISPLAY_NUM}" =~ ^:[0-9]+$ ]] || die "invalid --display '${DISPLAY_NUM}' (expected :N)"
[[ "${SCREEN_SPEC}" =~ ^[0-9]+x[0-9]+x[0-9]+$ ]] || die "invalid --screen '${SCREEN_SPEC}' (expected WxHxD)"
[[ "${WITH_SESSION_APPS}" -eq 0 || "${WITHOUT_SESSION_APPS}" -eq 0 ]] || die "--with-session-apps and --without-session-apps cannot be used together"
[[ "${SESSION_MODE}" == "xfce" || "${WITH_SESSION_APPS}" -eq 0 ]] || die "--with-session-apps requires --session xfce"

if [[ "${MODE}" == "system" && "${WITH_SESSION_APPS}" -eq 0 && "${WITHOUT_SESSION_APPS}" -eq 0 && -f "${UNIT_DIR}/praxi-desktop.service" ]]; then
  if grep -q '^Environment=PRAXI_ENABLE_SESSION_APPS=1$' "${UNIT_DIR}/praxi-desktop.service" 2>/dev/null; then
    KEEP_SESSION_APPS=1
  fi
fi
if [[ "${SESSION_MODE}" == "xfce" ]]; then
  if [[ "${WITH_SESSION_APPS}" -eq 1 || "${KEEP_SESSION_APPS}" -eq 1 ]] && [[ "${WITHOUT_SESSION_APPS}" -eq 0 ]]; then
    SESSION_APPS_ACTIVE=1
  fi
fi
if [[ "${SESSION_APPS_ACTIVE}" -eq 1 && "${MODE}" == "system" ]]; then
  warn "--with-session-apps enabled: xfce4-terminal and thunar run as '${SVC_USER}' and can read ${ENV_FILE} and ${DATA_DIR}, bypassing the daemon shell permission engine"
fi

if [[ "${DRY_RUN}" -eq 0 && "$(uname -s)" != "Linux" ]]; then
  die "setup-headless.sh must be run on a Linux host (or pass --dry-run)"
fi

if [[ "${MODE}" == "system" ]]; then
  if [[ "${DRY_RUN}" -eq 0 && "${EUID:-$(id -u)}" -ne 0 ]]; then
    die "--system requires root (run with sudo)"
  fi
else
  if [[ "${WITHOUT_DESKTOP}" -eq 1 ]]; then
    die "--without-desktop is only supported with --system"
  fi
  if [[ "${DRY_RUN}" -eq 0 && "${EUID:-$(id -u)}" -eq 0 ]]; then
    die "--user mode cannot be run as root; pass --system or run as an unprivileged user"
  fi
fi

ARCH="$(dpkg --print-architecture 2>/dev/null || uname -m)"
case "${ARCH}" in
  amd64|x86_64|arm64|aarch64)
    log "target architecture: ${ARCH}"
    ;;
  *)
    warn "untested architecture '${ARCH}', continuing..."
    ;;
esac

apt_candidate() {
  local candidate
  candidate="$(apt-cache policy "$1" 2>/dev/null | awk '/Candidate:/ {print $2}')"
  [[ -n "${candidate}" && "${candidate}" != "(none)" && "${candidate}" != *snap* ]]
}

find_non_snap_browser() {
  local candidate
  for candidate in "${BOT_BROWSER_PATH:-}" "${PRAXI_BROWSER_PATH:-}" /opt/praxi-browser/chrome /usr/bin/google-chrome /usr/bin/google-chrome-stable /usr/bin/chromium /usr/bin/chromium-browser; do
    [[ -n "${candidate}" && -x "${candidate}" ]] || continue
    if [[ "$(readlink -f "${candidate}")" == /snap/* ]] || head -c 8192 "${candidate}" | grep -aqE 'snap run|/snap/'; then
      continue
    fi
    printf '%s' "${candidate}"
    return 0
  done
  return 1
}

set_env_kv() {
  local file="$1" key="$2" val="$3"
  sed -i "/^${key}=/d" "${file}"
  printf '%s=%s\n' "${key}" "${val}" >> "${file}"
}

remove_env_key() {
  local file="$1" key="$2"
  sed -i "/^${key}=/d" "${file}"
}

install_desktop_packages() {
  local need_xfce=0
  if [[ "${SESSION_MODE}" == "xfce" ]]; then
    need_xfce=1
  fi
  if [[ "${WITHOUT_DESKTOP}" -eq 1 ]]; then
    return 0
  fi

  if command -v apt-get >/dev/null 2>&1; then
    # Set the variables after sudo so its environment policy cannot strip them.
    local -a apt_cmd=(env DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l NEEDRESTART_SUSPEND=1 apt-get)
    if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
      apt_cmd=(sudo "${apt_cmd[@]}")
    fi
    local pkgs_apt=(
      xvfb
      xauth
      dbus
      dbus-x11
      at-spi2-core
      x11-utils
      x11-xserver-utils
      xdg-utils
      fonts-liberation
      fonts-noto-core
    )
    if [[ "${need_xfce}" -eq 1 ]]; then
      pkgs_apt+=(
        xfwm4
        xfce4-panel
        xfdesktop4
        xfconf
        xfce4-settings
        desktop-file-utils
      )
      if [[ "${SESSION_APPS_ACTIVE}" -eq 1 ]]; then
        pkgs_apt+=(xfce4-terminal thunar)
      fi
    fi
    if [[ "${MODE}" == "user" ]]; then
      pkgs_apt+=(dbus-user-session)
    fi

    log "updating apt package index..."
    "${apt_cmd[@]}" update -y

    local pkg missing_pkgs=()
    for pkg in "${pkgs_apt[@]}"; do
      if ! apt_candidate "${pkg}"; then
        missing_pkgs+=("${pkg}")
      fi
    done
    if [[ "${#missing_pkgs[@]}" -gt 0 ]]; then
      die "missing required apt package candidate(s) on ${ARCH}: ${missing_pkgs[*]}"
    fi

    for pkg in libatk-adaptor libgail-common libglib2.0-bin tango-icon-theme fonts-noto-color-emoji fonts-noto-cjk; do
      if apt_candidate "${pkg}"; then
        pkgs_apt+=("${pkg}")
      else
        warn "optional package '${pkg}' has no candidate on ${ARCH}; continuing"
      fi
    done

    local distro_id=""
    if [[ -r /etc/os-release ]]; then
      # shellcheck source=/dev/null
      distro_id="$(. /etc/os-release; printf '%s' "${ID:-}")"
    fi
    if find_non_snap_browser >/dev/null 2>&1; then
      log "using existing non-snap browser at $(find_non_snap_browser)"
    elif [[ "${distro_id}" == "debian" ]] && apt_candidate chromium; then
      pkgs_apt+=(chromium)
    else
      warn "no non-snap apt chromium package found; install Chrome for Testing via scripts/install-linux.sh --with-desktop-session (see docs/guide/remote-host.md)"
    fi

    log "installing packages: ${pkgs_apt[*]}"
    "${apt_cmd[@]}" install -y --no-install-recommends "${pkgs_apt[@]}"
  elif command -v dnf >/dev/null 2>&1 && [[ "${MODE}" == "system" ]]; then
    local pkgs_dnf=(xorg-x11-server-Xvfb xauth dbus-daemon dbus-x11 at-spi2-core xdpyinfo xset xdg-utils liberation-fonts)
    if [[ "${need_xfce}" -eq 1 ]]; then
      pkgs_dnf+=(xfwm4 xfce4-panel xfdesktop xfconf xfce4-settings desktop-file-utils)
      if [[ "${SESSION_APPS_ACTIVE}" -eq 1 ]]; then
        pkgs_dnf+=(xfce4-terminal thunar)
      fi
    fi
    dnf install -y --setopt=install_weak_deps=False "${pkgs_dnf[@]}"
    for pkg in google-noto-emoji-color-fonts google-noto-sans-cjk-ttc-fonts; do
      dnf install -y --setopt=install_weak_deps=False "${pkg}" \
        || warn "could not install ${pkg}; emoji or CJK text in the browser may show empty boxes"
    done
  else
    die "supported package manager (apt-get, or dnf in --system mode) not found; pass --skip-packages if already installed"
  fi
}

write_xauth_helper() {
  local target="$1"
  cat > "${target}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
disp="${1:?display required}"
xauth_file="${2:?Xauthority path required}"
mkdir -p "$(dirname "${xauth_file}")"
if [[ -s "${xauth_file}" ]] && xauth -f "${xauth_file}" list "${disp}" 2>/dev/null | grep -q 'MIT-MAGIC-COOKIE-1'; then
  chmod 0600 "${xauth_file}"
  exit 0
fi
cookie=""
if command -v mcookie >/dev/null 2>&1; then
  cookie="$(mcookie)"
else
  cookie="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
fi
tmp_auth="${xauth_file}.tmp.$$"
trap 'rm -f "${tmp_auth}"' EXIT
umask 077
: > "${tmp_auth}"
chmod 0600 "${tmp_auth}"
xauth -f "${tmp_auth}" add "${disp}" . "${cookie}"
mv -f "${tmp_auth}" "${xauth_file}"
chmod 0600 "${xauth_file}"
EOF
  chmod 0755 "${target}"
}

write_wait_x11_helper() {
  local target="$1"
  cat > "${target}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
disp="${1:?display required}"
xauth_file="${2:?Xauthority path required}"
for _ in $(seq 1 100); do
  if DISPLAY="${disp}" XAUTHORITY="${xauth_file}" xdpyinfo >/dev/null 2>&1; then
    exit 0
  fi
  sleep 0.1
done
echo "error: Xvfb on ${disp} did not become ready within 10s" >&2
exit 1
EOF
  chmod 0755 "${target}"
}

write_wait_desktop_helper() {
  local target="$1"
  cat > "${target}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
dbus_socket="${1:?dbus socket path required}"
for _ in $(seq 1 100); do
  if [[ -S "${dbus_socket}" ]] && pgrep -u "$(id -u)" -x xfwm4 >/dev/null 2>&1; then
    exit 0
  fi
  sleep 0.1
done
echo "error: Xfce desktop session did not become ready within 10s" >&2
exit 1
EOF
  chmod 0755 "${target}"
}

write_xvfb_uphold_helper() {
  local target="$1"
  cat > "${target}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
# Fallback for systemd < 249 (no Upholds=): start enabled dependents after
# Xvfb starts or restarts so they rejoin the new Xvfb mount namespace.
for u in praxi-desktop.service praxi.service praxi-node.service; do
  if systemctl is-enabled --quiet "${u}" 2>/dev/null; then
    systemctl start --no-block "${u}" >/dev/null 2>&1 || true
  fi
done
EOF
  chmod 0755 "${target}"
}

write_xfce_session_script() {
  local target="$1"
  cat > "${target}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

: "${DISPLAY:?DISPLAY is required}"
: "${XAUTHORITY:?XAUTHORITY is required}"
: "${HOME:?HOME is required}"

DBUS_SOCKET="${PRAXI_DBUS_SOCKET:-${HOME}/.dbus-session}"
export DBUS_SESSION_BUS_ADDRESS="unix:path=${DBUS_SOCKET}"
export XDG_SESSION_TYPE="x11"
export XDG_CURRENT_DESKTOP="XFCE"
export XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME}/.config}"
export XDG_DATA_HOME="${XDG_DATA_HOME:-${HOME}/.local/share}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-${HOME}/.cache}"
export NO_AT_BRIDGE=0
export GTK_MODULES="gail:atk-bridge"
export QT_ACCESSIBILITY=1
export QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
export GNOME_ACCESSIBILITY=1

mkdir -p \
  "${XDG_CONFIG_HOME}/autostart" \
  "${XDG_CONFIG_HOME}/xfce4/xfconf/xfce-perchannel-xml" \
  "${XDG_DATA_HOME}/applications" \
  "${XDG_CACHE_HOME}"

# Disable lock/power/printer/keyring/network-tray autostarts unconditionally.
for desktop_name in \
  xfce4-screensaver.desktop \
  light-locker.desktop \
  xscreensaver.desktop \
  xfce4-power-manager.desktop \
  print-applet.desktop \
  system-config-printer.desktop \
  blueman.desktop \
  nm-applet.desktop \
  update-notifier.desktop \
  gnome-keyring-pkcs11.desktop \
  gnome-keyring-secrets.desktop \
  gnome-keyring-ssh.desktop \
  org.gnome.Evolution-alarm-notify.desktop; do
  cat > "${XDG_CONFIG_HOME}/autostart/${desktop_name}" <<'AUTOSTART'
[Desktop Entry]
Type=Application
Hidden=true
AUTOSTART
done

# Seed minimal Xfce configuration without clobbering later user tweaks,
# except compositing stays disabled below via --compositor=off + xfconf-query.
xfconf_dir="${XDG_CONFIG_HOME}/xfce4/xfconf/xfce-perchannel-xml"
if [[ ! -f "${xfconf_dir}/xfwm4.xml" ]]; then
  cat > "${xfconf_dir}/xfwm4.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<channel name="xfwm4" version="1.0">
  <property name="general" type="empty">
    <property name="use_compositing" type="bool" value="false"/>
    <property name="workspace_count" type="int" value="1"/>
    <property name="snap_to_border" type="bool" value="true"/>
    <property name="snap_to_windows" type="bool" value="false"/>
  </property>
</channel>
XML
fi

if [[ ! -f "${xfconf_dir}/xfce4-panel.xml" ]]; then
  cat > "${xfconf_dir}/xfce4-panel.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<channel name="xfce4-panel" version="1.0">
  <property name="configver" type="int" value="2"/>
  <property name="panels" type="array">
    <value type="int" value="1"/>
    <property name="dark-mode" type="bool" value="true"/>
    <property name="panel-1" type="empty">
      <property name="position" type="string" value="p=6;x=0;y=0"/>
      <property name="length" type="uint" value="100"/>
      <property name="position-locked" type="bool" value="true"/>
      <property name="size" type="uint" value="28"/>
      <property name="plugin-ids" type="array">
        <value type="int" value="1"/>
        <value type="int" value="2"/>
        <value type="int" value="3"/>
        <value type="int" value="4"/>
      </property>
    </property>
  </property>
  <property name="plugins" type="empty">
    <property name="plugin-1" type="string" value="applicationsmenu"/>
    <property name="plugin-2" type="string" value="tasklist"/>
    <property name="plugin-3" type="string" value="separator">
      <property name="expand" type="bool" value="true"/>
      <property name="style" type="uint" value="0"/>
    </property>
    <property name="plugin-4" type="string" value="clock"/>
  </property>
</channel>
XML
fi

if [[ ! -f "${xfconf_dir}/xfce4-desktop.xml" ]]; then
  cat > "${xfconf_dir}/xfce4-desktop.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<channel name="xfce4-desktop" version="1.0">
  <property name="backdrop" type="empty">
    <property name="screen0" type="empty">
      <property name="monitorscreen" type="empty">
        <property name="workspace0" type="empty">
          <property name="color-style" type="int" value="0"/>
          <property name="image-style" type="int" value="0"/>
          <property name="rgba1" type="array">
            <value type="double" value="0.117647"/>
            <value type="double" value="0.141176"/>
            <value type="double" value="0.188235"/>
            <value type="double" value="1.000000"/>
          </property>
        </property>
      </property>
    </property>
  </property>
  <property name="desktop-icons" type="empty">
    <property name="style" type="int" value="2"/>
    <property name="file-icons" type="empty">
      <property name="show-trash" type="bool" value="false"/>
      <property name="show-removable" type="bool" value="false"/>
    </property>
  </property>
</channel>
XML
fi

# Register a manual browser menu launcher when a non-snap browser is installed.
# Never autostarted; opened only on explicit human menu action.
browser_bin=""
for candidate in "${BOT_BROWSER_PATH:-}" "${PRAXI_BROWSER_PATH:-}" /opt/praxi-browser/chrome /usr/bin/google-chrome /usr/bin/google-chrome-stable /usr/bin/chromium /usr/bin/chromium-browser; do
  [[ -n "${candidate}" && -x "${candidate}" ]] || continue
  if [[ "$(readlink -f "${candidate}")" == /snap/* ]] || head -c 8192 "${candidate}" | grep -aqE 'snap run|/snap/'; then
    continue
  fi
  browser_bin="${candidate}"
  break
done
if [[ -n "${browser_bin}" ]]; then
  browser_icon="web-browser"
  [[ ! -f "${browser_bin%/*}/product_logo_48.png" ]] || browser_icon="${browser_bin%/*}/product_logo_48.png"
  cat > "${XDG_DATA_HOME}/applications/praxi-browser.desktop" <<DESKTOP
[Desktop Entry]
Version=1.0
Type=Application
Name=Web Browser
Comment=Open the web browser (opened only by a person, never autostarted)
Exec=${browser_bin} --no-first-run --no-default-browser-check --disable-dev-shm-usage --disable-gpu %U
Icon=${browser_icon}
Terminal=false
Categories=Network;WebBrowser;
StartupNotify=true
DESKTOP
  # Also put it on the desktop so a person watching the live view sees it.
  # Executable .desktop files on ~/Desktop open without a trust prompt in Xfce.
  mkdir -p "${HOME}/Desktop"
  cp "${XDG_DATA_HOME}/applications/praxi-browser.desktop" "${HOME}/Desktop/praxi-browser.desktop"
  chmod 0755 "${HOME}/Desktop/praxi-browser.desktop"
fi
if [[ "${PRAXI_ENABLE_SESSION_APPS:-0}" == "1" ]]; then
  for app_desktop in xfce4-terminal.desktop thunar.desktop; do
    if [[ -f "${XDG_DATA_HOME}/applications/${app_desktop}" ]] && grep -q '^Hidden=true$' "${XDG_DATA_HOME}/applications/${app_desktop}"; then
      rm -f "${XDG_DATA_HOME}/applications/${app_desktop}"
    fi
  done
  if [[ ! -f "${XDG_CONFIG_HOME}/xfce4/helpers.rc" ]]; then
    {
      echo "TerminalEmulator=xfce4-terminal"
      echo "FileManager=thunar"
      if [[ -n "${browser_bin}" ]]; then
        echo "WebBrowser=praxi-browser"
      fi
    } > "${XDG_CONFIG_HOME}/xfce4/helpers.rc"
  fi
else
  for app_desktop in xfce4-terminal.desktop thunar.desktop; do
    cat > "${XDG_DATA_HOME}/applications/${app_desktop}" <<'APP_HIDE'
[Desktop Entry]
Type=Application
Hidden=true
APP_HIDE
  done
  if [[ ! -f "${XDG_CONFIG_HOME}/xfce4/helpers.rc" ]] && [[ -n "${browser_bin}" ]]; then
    echo "WebBrowser=praxi-browser" > "${XDG_CONFIG_HOME}/xfce4/helpers.rc"
  fi
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${XDG_DATA_HOME}/applications" >/dev/null 2>&1 || true
fi

rm -f "${DBUS_SOCKET}"
DBUS_PID=""
XFSETTINGSD_PID=""
XFWM_PID=""
XFDESKTOP_PID=""
XFPANEL_PID=""
STOP_REQUESTED=0

cleanup() {
  local pid
  for pid in "${XFPANEL_PID}" "${XFDESKTOP_PID}" "${XFWM_PID}" "${XFSETTINGSD_PID}" "${DBUS_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill -TERM "${pid}" 2>/dev/null || true
    fi
  done
  wait 2>/dev/null || true
  rm -f "${DBUS_SOCKET}"
}
on_term() {
  STOP_REQUESTED=1
  cleanup
  exit 0
}
trap on_term INT TERM
trap cleanup EXIT

dbus-daemon --session --address="unix:path=${DBUS_SOCKET}" --nofork --nopidfile &
DBUS_PID=$!

for _ in $(seq 1 50); do
  if [[ -S "${DBUS_SOCKET}" ]]; then
    chmod 0600 "${DBUS_SOCKET}"
    break
  fi
  sleep 0.1
done
[[ -S "${DBUS_SOCKET}" ]] || { echo "error: session D-Bus socket did not appear" >&2; exit 1; }
chmod 0600 "${DBUS_SOCKET}"

if command -v dbus-update-activation-environment >/dev/null 2>&1; then
  dbus-update-activation-environment \
    DISPLAY XAUTHORITY DBUS_SESSION_BUS_ADDRESS XDG_SESSION_TYPE XDG_CURRENT_DESKTOP \
    NO_AT_BRIDGE GTK_MODULES QT_ACCESSIBILITY QT_LINUX_ACCESSIBILITY_ALWAYS_ON GNOME_ACCESSIBILITY || true
fi

if [[ -x /usr/libexec/at-spi-bus-launcher ]]; then
  /usr/libexec/at-spi-bus-launcher --launch-immediately >/dev/null 2>&1 &
elif [[ -x /usr/lib/at-spi2-core/at-spi-bus-launcher ]]; then
  /usr/lib/at-spi2-core/at-spi-bus-launcher --launch-immediately >/dev/null 2>&1 &
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

if command -v xfconf-query >/dev/null 2>&1; then
  xfconf-query -c xfwm4 -p /general/use_compositing -n -t bool -s false >/dev/null 2>&1 || true
  xfconf-query -c xfwm4 -p /general/use_compositing -s false >/dev/null 2>&1 || true
fi

if command -v xset >/dev/null 2>&1; then
  xset s off -dpms >/dev/null 2>&1 || true
fi

if command -v xfsettingsd >/dev/null 2>&1; then
  xfsettingsd --no-daemon >/dev/null 2>&1 &
  XFSETTINGSD_PID=$!
fi

xfwm4 --compositor=off &
XFWM_PID=$!

xfdesktop &
XFDESKTOP_PID=$!

xfce4-panel &
XFPANEL_PID=$!

while [[ "${STOP_REQUESTED}" -eq 0 ]]; do
  rc=0
  wait -n "${DBUS_PID}" "${XFWM_PID}" "${XFDESKTOP_PID}" "${XFPANEL_PID}" || rc=$?
  [[ "${STOP_REQUESTED}" -eq 0 ]] || break
  if ! kill -0 "${DBUS_PID}" 2>/dev/null; then
    echo "error: session dbus-daemon exited (rc=${rc})" >&2
    exit 1
  fi
  if ! kill -0 "${XFWM_PID}" 2>/dev/null; then
    echo "error: xfwm4 exited (rc=${rc})" >&2
    exit 1
  fi
  if ! kill -0 "${XFDESKTOP_PID}" 2>/dev/null; then
    echo "warning: xfdesktop exited (rc=${rc}); restarting" >&2
    sleep 1
    [[ "${STOP_REQUESTED}" -eq 0 ]] || break
    xfdesktop &
    XFDESKTOP_PID=$!
  fi
  if ! kill -0 "${XFPANEL_PID}" 2>/dev/null; then
    echo "warning: xfce4-panel exited (rc=${rc}); restarting" >&2
    sleep 1
    [[ "${STOP_REQUESTED}" -eq 0 ]] || break
    xfce4-panel &
    XFPANEL_PID=$!
  fi
done
EOF
  chmod 0755 "${target}"
}

if [[ "${DRY_RUN}" -eq 1 ]]; then
  log "[dry-run] mode=${MODE} session=${SESSION_MODE} without_desktop=${WITHOUT_DESKTOP} display=${DISPLAY_NUM} screen=${SCREEN_SPEC} skip_packages=${SKIP_PACKAGES} no_start=${NO_START}"
  if [[ "${MODE}" == "system" ]]; then
    log "[dry-run] slice=${UNIT_DIR}/${SLICE_NAME} (CPUQuota=100%, MemoryHigh=3G, MemoryMax=4G)"
    log "[dry-run] xvfb_unit=${UNIT_DIR}/praxi-xvfb.service (User=${SVC_USER}, -auth ${XAUTH_FILE}, -nolisten tcp)"
    if [[ "${SESSION_MODE}" == "xfce" ]]; then
      log "[dry-run] desktop_unit=${UNIT_DIR}/praxi-desktop.service (User=${SVC_USER}, Xfce minimal without compositor/locker/power)"
    fi
  else
    log "[dry-run] user_unit=${HOME}/.config/systemd/user/praxi-headless-desktop.service (Xfce minimal, private Xauthority, no VNC)"
  fi
  exit 0
fi

if [[ "${SKIP_PACKAGES}" -eq 0 ]]; then
  install_desktop_packages
fi

if [[ "${WITHOUT_DESKTOP}" -eq 0 ]]; then
  for req_cmd in Xvfb xauth xdpyinfo; do
    command -v "${req_cmd}" >/dev/null 2>&1 || die "${req_cmd} not found (install desktop prerequisites or drop --skip-packages)"
  done
fi

# ------------------------------------------------------------------ system mode
if [[ "${MODE}" == "system" ]]; then
  if ! id -u "${SVC_USER}" >/dev/null 2>&1; then
    die "system user '${SVC_USER}' does not exist; install Praxi first with scripts/install-linux.sh"
  fi
  install -d -m 0750 -o "${SVC_USER}" -g "${SVC_USER}" "${DATA_DIR}"
  install -d -m 0750 -o root -g "${SVC_USER}" "${CONF_DIR}"

  run_user_systemctl() {
    local uname="$1" uid="$2"
    shift 2
    local runtime_dir="/run/user/${uid}"
    local current_user
    current_user="$(id -un 2>/dev/null || true)"
    if [[ -z "${uname}" || "${uname}" == "${current_user}" || "${EUID:-$(id -u)}" -ne 0 ]]; then
      env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR="${runtime_dir}" systemctl --user "$@" >/dev/null 2>&1
    elif command -v runuser >/dev/null 2>&1; then
      runuser -u "${uname}" -- env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR="${runtime_dir}" systemctl --user "$@" >/dev/null 2>&1
    else
      warn "runuser not available; cannot run systemctl --user for ${uname}"
      return 1
    fi
  }

  # Clean up any conflicting user-level unit on the invoking user's account.
  cleanup_user_unit() {
    local uname="$1" uhome="$2"
    [[ -n "${uhome}" ]] || return 0
    local uunit="${uhome}/.config/systemd/user/praxi-headless-desktop.service"
    local uvnc="${uhome}/.config/systemd/user/praxi-headless-vnc.service"
    local uid="" match_display=0 exec_script=""
    if [[ -n "${uname}" ]]; then
      uid="$(id -u "${uname}" 2>/dev/null || true)"
    fi
    if [[ -f "${uvnc}" ]]; then
      if [[ -n "${uid}" ]]; then
        run_user_systemctl "${uname}" "${uid}" disable --now praxi-headless-vnc.service || warn "could not stop/disable ${uvnc} via systemctl --user"
      fi
      rm -f "${uvnc}" "${uhome}/.config/systemd/user/default.target.wants/praxi-headless-vnc.service"
      [[ -n "${uid}" ]] && { run_user_systemctl "${uname}" "${uid}" daemon-reload || true; }
    fi
    if [[ -f "${uunit}" ]]; then
      if grep -Eq "DISPLAY=([\"\']?)${DISPLAY_NUM}([\"\'[:space:]]|$)" "${uunit}" 2>/dev/null; then
        match_display=1
      else
        exec_script="$(awk -F= '/^ExecStart=/ {sub(/^[[:space:]]*/, "", $2); split($2, a, /[[:space:]]+/); print a[1]; exit}' "${uunit}" 2>/dev/null || true)"
        if [[ -n "${exec_script}" && -f "${exec_script}" ]] && grep -Eq "DISPLAY=([\"\']?)${DISPLAY_NUM}([\"\'[:space:]]|$)" "${exec_script}" 2>/dev/null; then
          match_display=1
        fi
      fi
    fi
    if [[ "${match_display}" -eq 1 ]]; then
      log "stopping, disabling, and removing conflicting user-level unit ${uunit} for ${DISPLAY_NUM}"
      if [[ -n "${uid}" ]]; then
        if run_user_systemctl "${uname}" "${uid}" is-active --quiet praxi-headless-desktop.service; then
          if ! run_user_systemctl "${uname}" "${uid}" disable --now praxi-headless-desktop.service; then
            warn "could not stop active user unit praxi-headless-desktop.service for ${uname} via systemctl --user"
          fi
        else
          run_user_systemctl "${uname}" "${uid}" disable praxi-headless-desktop.service || true
        fi
      fi
      rm -f "${uunit}" "${uhome}/.config/systemd/user/default.target.wants/praxi-headless-desktop.service"
      [[ -n "${uid}" ]] && { run_user_systemctl "${uname}" "${uid}" daemon-reload || true; }
    fi
  }
  if [[ "${WITHOUT_DESKTOP}" -eq 0 ]]; then
    cur_user="$(id -un 2>/dev/null || true)"
    cleanup_user_unit "${cur_user}" "${HOME}"
    if [[ -n "${SUDO_USER:-}" ]]; then
      sudo_home="$(getent passwd "${SUDO_USER}" | cut -d: -f6 || true)"
      if [[ -n "${sudo_home}" && "${sudo_home}" != "${HOME}" ]]; then
        cleanup_user_unit "${SUDO_USER}" "${sudo_home}"
      fi
    fi
    if [[ "${DATA_DIR}" != "${HOME}" ]]; then
      cleanup_user_unit "${SVC_USER}" "${DATA_DIR}"
    fi
  fi

  HARDENING="NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=${DATA_DIR}
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
ProtectClock=true
RestrictSUIDSGID=true
RestrictRealtime=true
LockPersonality=true
SystemCallArchitectures=native
UMask=0077
LogRateLimitIntervalSec=30s
LogRateLimitBurst=2000"

# Not set on purpose: MemoryDenyWriteExecute and RestrictNamespaces (Chromium needs them).

  if [[ "${WITHOUT_DESKTOP}" -eq 1 ]]; then
    log "removing praxi-desktop.service, praxi-xvfb.service, and ${SLICE_NAME}"
    systemctl disable --now praxi-xvfb.service praxi-desktop.service >/dev/null 2>&1 || true
    rm -f "${UNIT_DIR}/praxi-desktop.service" "${UNIT_DIR}/praxi-xvfb.service"
    rm -f "${UNIT_DIR}/praxi.service.d/desktop.conf" "${UNIT_DIR}/praxi-node.service.d/desktop.conf"
    for svc in praxi praxi-node; do
      if [[ -f "${UNIT_DIR}/${svc}.service" ]]; then
        sed -i "/^Slice=${SLICE_NAME}$/d" "${UNIT_DIR}/${svc}.service"
      fi
    done
    if [[ -f /usr/local/bin/praxi ]]; then
      sed -i "s|--property=Slice=${SLICE_NAME} ||g" /usr/local/bin/praxi
    fi
    systemctl disable --now "${SLICE_NAME}" >/dev/null 2>&1 || true
    rm -f "${UNIT_DIR}/${SLICE_NAME}"
    if [[ -f "${ENV_FILE}" ]]; then
      for k in DISPLAY XAUTHORITY DBUS_SESSION_BUS_ADDRESS XDG_SESSION_TYPE NO_AT_BRIDGE GTK_MODULES QT_ACCESSIBILITY QT_LINUX_ACCESSIBILITY_ALWAYS_ON GNOME_ACCESSIBILITY; do
        remove_env_key "${ENV_FILE}" "${k}"
      done
    fi
    if [[ -f "${NODE_ENV_FILE}" ]]; then
      for k in DISPLAY XAUTHORITY DBUS_SESSION_BUS_ADDRESS XDG_SESSION_TYPE NO_AT_BRIDGE GTK_MODULES QT_ACCESSIBILITY QT_LINUX_ACCESSIBILITY_ALWAYS_ON GNOME_ACCESSIBILITY; do
        remove_env_key "${NODE_ENV_FILE}" "${k}"
      done
    fi
    systemctl daemon-reload
    if [[ "${NO_START}" -eq 0 ]] && systemctl is-enabled --quiet praxi.service 2>/dev/null; then
      systemctl restart praxi.service
    elif [[ "${NO_START}" -eq 1 ]] && systemctl is-enabled --quiet praxi.service 2>/dev/null; then
      warn "praxi.service was not restarted (--no-start); start it with: sudo systemctl restart praxi.service"
    fi
    log "system desktop units removed."
    exit 0
  fi

  cat > "${UNIT_DIR}/${SLICE_NAME}" <<UNIT
[Unit]
Description=Praxi-Bot resource containment slice

[Slice]
CPUAccounting=true
MemoryAccounting=true
TasksAccounting=true
CPUQuota=100%
MemoryHigh=3G
MemoryMax=4G
UNIT

  xvfb_bin="$(command -v Xvfb || true)"
  [[ -n "${xvfb_bin}" ]] || die "Xvfb not found (install it or drop --skip-packages)"
  browser_bin="$(find_non_snap_browser || true)"

  install -d -m 0755 "${LIBEXEC_DIR}"
  write_xauth_helper "${LIBEXEC_DIR}/praxi-xauth-init.sh"
  write_wait_x11_helper "${LIBEXEC_DIR}/praxi-wait-x11.sh"
  write_wait_desktop_helper "${LIBEXEC_DIR}/praxi-wait-desktop.sh"
  write_xfce_session_script "${LIBEXEC_DIR}/praxi-xfce-session.sh"
  write_xvfb_uphold_helper "${LIBEXEC_DIR}/praxi-xvfb-uphold.sh"

  SYSTEMD_VER="$(systemctl --version 2>/dev/null | awk 'NR==1 {print $2}' | sed 's/[^0-9].*//')"
  SYSTEMD_HAS_UPHOLDS=0
  if [[ "${SYSTEMD_VER}" =~ ^[0-9]+$ ]] && (( SYSTEMD_VER >= 249 )); then
    SYSTEMD_HAS_UPHOLDS=1
  else
    warn "systemd version '${SYSTEMD_VER:-unknown}' (< 249) does not support Upholds=; omitting Upholds= from praxi-xvfb.service and using ExecStartPost uphold fallback for dependent recovery"
  fi
  upholds_line=""
  uphold_post_line=""
  if [[ "${SYSTEMD_HAS_UPHOLDS}" -eq 1 ]]; then
    if [[ "${SESSION_MODE}" == "xfce" ]]; then
      upholds_line="Upholds=praxi-desktop.service praxi.service"
    else
      upholds_line="Upholds=praxi.service"
    fi
  else
    uphold_post_line="ExecStartPost=!${LIBEXEC_DIR}/praxi-xvfb-uphold.sh"
  fi

  cat > "${UNIT_DIR}/praxi-xvfb.service" <<UNIT
[Unit]
Description=Praxi virtual display (Xvfb ${DISPLAY_NUM})
${upholds_line:+${upholds_line}
}
[Service]
Type=simple
Slice=${SLICE_NAME}
User=${SVC_USER}
Group=${SVC_USER}
WorkingDirectory=${DATA_DIR}
Environment=HOME=${DATA_DIR}
Environment=DISPLAY=${DISPLAY_NUM}
Environment=XAUTHORITY=${XAUTH_FILE}
# '!' bypasses User/Group only, preserving PrivateTmp for this root helper.
ExecStartPre=!/usr/bin/install -d -o root -g root -m 1777 /tmp/.X11-unix
ExecStartPre=${LIBEXEC_DIR}/praxi-xauth-init.sh ${DISPLAY_NUM} ${XAUTH_FILE}
ExecStart=${xvfb_bin} ${DISPLAY_NUM} -screen 0 ${SCREEN_SPEC} -auth ${XAUTH_FILE} -nolisten tcp +extension RANDR +extension XTEST
ExecStartPost=${LIBEXEC_DIR}/praxi-wait-x11.sh ${DISPLAY_NUM} ${XAUTH_FILE}
${uphold_post_line:+${uphold_post_line}
}Restart=on-failure
RestartSec=3
TimeoutStopSec=15
${HARDENING}

[Install]
WantedBy=multi-user.target
UNIT

  if [[ "${SESSION_MODE}" == "xfce" ]]; then
    cat > "${UNIT_DIR}/praxi-desktop.service" <<UNIT
[Unit]
Description=Praxi minimal Xfce desktop session (${DISPLAY_NUM})
BindsTo=praxi-xvfb.service
After=praxi-xvfb.service
PartOf=praxi-xvfb.service
JoinsNamespaceOf=praxi-xvfb.service
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
Type=simple
Slice=${SLICE_NAME}
User=${SVC_USER}
Group=${SVC_USER}
WorkingDirectory=${DATA_DIR}
Environment=HOME=${DATA_DIR}
Environment=DISPLAY=${DISPLAY_NUM}
Environment=XAUTHORITY=${XAUTH_FILE}
Environment=PRAXI_DBUS_SOCKET=${DBUS_SOCKET}
Environment=DBUS_SESSION_BUS_ADDRESS=unix:path=${DBUS_SOCKET}
Environment=XDG_SESSION_TYPE=x11
Environment=XDG_CURRENT_DESKTOP=XFCE
Environment=XDG_CONFIG_HOME=${DATA_DIR}/.config
Environment=XDG_DATA_HOME=${DATA_DIR}/.local/share
Environment=XDG_CACHE_HOME=${DATA_DIR}/.cache
Environment=NO_AT_BRIDGE=0
Environment=GTK_MODULES=gail:atk-bridge
Environment=QT_ACCESSIBILITY=1
Environment=QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
Environment=GNOME_ACCESSIBILITY=1
Environment=PRAXI_ENABLE_SESSION_APPS=${SESSION_APPS_ACTIVE}
${browser_bin:+Environment=BOT_BROWSER_PATH=${browser_bin}}
ExecStart=${LIBEXEC_DIR}/praxi-xfce-session.sh
ExecStartPost=${LIBEXEC_DIR}/praxi-wait-desktop.sh ${DBUS_SOCKET}
Restart=always
RestartSec=3
TimeoutStopSec=15
KillMode=control-group
${HARDENING}

[Install]
WantedBy=multi-user.target
UNIT
  else
    systemctl daemon-reload
    systemctl disable --now praxi-desktop.service >/dev/null 2>&1 || true
    rm -f "${UNIT_DIR}/praxi-desktop.service"
  fi

  # Apply Slice=praxi.slice to existing praxi.service / praxi-node.service if present
  for svc in praxi praxi-node; do
    if [[ -f "${UNIT_DIR}/${svc}.service" ]]; then
      if grep -q '^Slice=' "${UNIT_DIR}/${svc}.service"; then
        sed -i "s/^Slice=.*/Slice=${SLICE_NAME}/" "${UNIT_DIR}/${svc}.service"
      else
        sed -i "/^\[Service\]/a Slice=${SLICE_NAME}" "${UNIT_DIR}/${svc}.service"
      fi
    fi
    install -d "${UNIT_DIR}/${svc}.service.d"
    # Share the private /tmp (X11 socket) with the display service.
    # Use Wants= for praxi-desktop.service so a desktop-session failure does
    # not block the always-on daemon.
    if [[ "${SESSION_MODE}" == "xfce" ]]; then
      cat > "${UNIT_DIR}/${svc}.service.d/desktop.conf" <<UNIT
[Unit]
BindsTo=praxi-xvfb.service
Wants=praxi-desktop.service
After=praxi-xvfb.service praxi-desktop.service
JoinsNamespaceOf=praxi-xvfb.service
UNIT
    else
      cat > "${UNIT_DIR}/${svc}.service.d/desktop.conf" <<UNIT
[Unit]
BindsTo=praxi-xvfb.service
After=praxi-xvfb.service
JoinsNamespaceOf=praxi-xvfb.service
UNIT
    fi
  done

  if [[ -f "${ENV_FILE}" ]]; then
    set_env_kv "${ENV_FILE}" DISPLAY "${DISPLAY_NUM}"
    set_env_kv "${ENV_FILE}" XAUTHORITY "${XAUTH_FILE}"
    if [[ "${SESSION_MODE}" == "xfce" ]]; then
      set_env_kv "${ENV_FILE}" DBUS_SESSION_BUS_ADDRESS "unix:path=${DBUS_SOCKET}"
      set_env_kv "${ENV_FILE}" XDG_SESSION_TYPE x11
      set_env_kv "${ENV_FILE}" NO_AT_BRIDGE 0
      set_env_kv "${ENV_FILE}" GTK_MODULES "gail:atk-bridge"
      set_env_kv "${ENV_FILE}" QT_ACCESSIBILITY 1
      set_env_kv "${ENV_FILE}" QT_LINUX_ACCESSIBILITY_ALWAYS_ON 1
      set_env_kv "${ENV_FILE}" GNOME_ACCESSIBILITY 1
    else
      for k in DBUS_SESSION_BUS_ADDRESS XDG_SESSION_TYPE NO_AT_BRIDGE GTK_MODULES QT_ACCESSIBILITY QT_LINUX_ACCESSIBILITY_ALWAYS_ON GNOME_ACCESSIBILITY; do
        remove_env_key "${ENV_FILE}" "${k}"
      done
    fi
    chown root:"${SVC_USER}" "${ENV_FILE}"
    chmod 0640 "${ENV_FILE}"
  fi

  if [[ -f "${NODE_ENV_FILE}" ]]; then
    set_env_kv "${NODE_ENV_FILE}" DISPLAY "${DISPLAY_NUM}"
    set_env_kv "${NODE_ENV_FILE}" XAUTHORITY "${XAUTH_FILE}"
    if [[ "${SESSION_MODE}" == "xfce" ]]; then
      set_env_kv "${NODE_ENV_FILE}" DBUS_SESSION_BUS_ADDRESS "unix:path=${DBUS_SOCKET}"
      set_env_kv "${NODE_ENV_FILE}" XDG_SESSION_TYPE x11
      set_env_kv "${NODE_ENV_FILE}" NO_AT_BRIDGE 0
      set_env_kv "${NODE_ENV_FILE}" GTK_MODULES "gail:atk-bridge"
      set_env_kv "${NODE_ENV_FILE}" QT_ACCESSIBILITY 1
      set_env_kv "${NODE_ENV_FILE}" QT_LINUX_ACCESSIBILITY_ALWAYS_ON 1
      set_env_kv "${NODE_ENV_FILE}" GNOME_ACCESSIBILITY 1
    else
      for k in DBUS_SESSION_BUS_ADDRESS XDG_SESSION_TYPE NO_AT_BRIDGE GTK_MODULES QT_ACCESSIBILITY QT_LINUX_ACCESSIBILITY_ALWAYS_ON GNOME_ACCESSIBILITY; do
        remove_env_key "${NODE_ENV_FILE}" "${k}"
      done
    fi
    if grep -q '^BOT_SECRETS_PASSPHRASE=' "${NODE_ENV_FILE}"; then
      die "refusing to leave BOT_SECRETS_PASSPHRASE in ${NODE_ENV_FILE}"
    fi
    chown root:"${SVC_USER}" "${NODE_ENV_FILE}"
    chmod 0640 "${NODE_ENV_FILE}"
  fi

  # Ensure /usr/local/bin/praxi doctor transient unit also shares praxi.slice
  if [[ -f /usr/local/bin/praxi ]] && ! grep -q "Slice=${SLICE_NAME}" /usr/local/bin/praxi; then
    sed -i "s|--property=User=${SVC_USER}|--property=Slice=${SLICE_NAME} --property=User=${SVC_USER}|" /usr/local/bin/praxi
  fi

  systemctl daemon-reload
  systemctl enable praxi-xvfb.service >/dev/null
  if [[ "${SESSION_MODE}" == "xfce" ]]; then
    systemctl enable praxi-desktop.service >/dev/null
  fi

  if [[ "${NO_START}" -eq 0 ]]; then
    systemctl restart praxi-xvfb.service
    if [[ "${SESSION_MODE}" == "xfce" ]] && ! systemctl is-active --quiet praxi-desktop.service 2>/dev/null; then
      systemctl start praxi-desktop.service
    fi
    if systemctl is-enabled --quiet praxi.service 2>/dev/null && ! systemctl is-active --quiet praxi.service 2>/dev/null; then
      systemctl start praxi.service
    fi
    if systemctl is-active --quiet praxi-node.service 2>/dev/null; then
      systemctl restart praxi-node.service
    fi
    log "system desktop services active (slice=${SLICE_NAME}, display=${DISPLAY_NUM}, screen=${SCREEN_SPEC}, session=${SESSION_MODE})"
  else
    log "system desktop units written and enabled (--no-start)"
    if [[ "${SESSION_MODE}" == "xfce" ]]; then
      warn "desktop/daemon services were not started (--no-start); start them with: sudo systemctl start praxi-xvfb.service praxi-desktop.service praxi.service"
    else
      warn "display/daemon services were not started (--no-start); start them with: sudo systemctl start praxi-xvfb.service praxi.service"
    fi
  fi
  exit 0
fi

# -------------------------------------------------------------------- user mode
# Never start a duplicate display server on a display owned by praxi-xvfb.service.
if [[ -f "${UNIT_DIR}/praxi-xvfb.service" ]] && grep -qE "(^|[[:space:]])${DISPLAY_NUM}([[:space:]]|$)" "${UNIT_DIR}/praxi-xvfb.service" 2>/dev/null; then
  die "display ${DISPLAY_NUM} is owned by system unit praxi-xvfb.service; pass a different --display (e.g. :98) for user dev mode, or use --system"
fi

BIN_DIR="${HOME}/.local/bin"
SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
SESSION_SCRIPT="${BIN_DIR}/praxi-headless-session.sh"
DESKTOP_UNIT="${SYSTEMD_USER_DIR}/praxi-headless-desktop.service"
VNC_UNIT="${SYSTEMD_USER_DIR}/praxi-headless-vnc.service"
USER_XAUTH="${HOME}/.praxi-headless-Xauthority-${DISPLAY_NUM#:}"
USER_DBUS="${HOME}/.praxi-headless-dbus-${DISPLAY_NUM#:}"

mkdir -p "${BIN_DIR}" "${SYSTEMD_USER_DIR}"
write_xauth_helper "${BIN_DIR}/praxi-xauth-init.sh"
write_xfce_session_script "${BIN_DIR}/praxi-xfce-session.sh"

if [[ -f "${VNC_UNIT}" ]]; then
  systemctl --user disable --now praxi-headless-vnc.service 2>/dev/null || true
  rm -f "${VNC_UNIT}"
fi

cat > "${SESSION_SCRIPT}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

export DISPLAY="${DISPLAY_NUM}"
export XAUTHORITY="${USER_XAUTH}"
export PRAXI_DBUS_SOCKET="${USER_DBUS}"
export PRAXI_ENABLE_SESSION_APPS="${SESSION_APPS_ACTIVE}"
export HOME="${HOME}"

XVFB_PID=""
SESSION_PID=""

cleanup() {
  if [[ -n "\${SESSION_PID}" ]] && kill -0 "\${SESSION_PID}" 2>/dev/null; then
    kill -TERM "\${SESSION_PID}" 2>/dev/null || true
  fi
  if [[ -n "\${XVFB_PID}" ]] && kill -0 "\${XVFB_PID}" 2>/dev/null; then
    kill -TERM "\${XVFB_PID}" 2>/dev/null || true
  fi
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

"${BIN_DIR}/praxi-xauth-init.sh" "\${DISPLAY}" "\${XAUTHORITY}"
Xvfb "\${DISPLAY}" -screen 0 "${SCREEN_SPEC}" -auth "\${XAUTHORITY}" -nolisten tcp +extension RANDR +extension XTEST &
XVFB_PID=\$!

for _ in \$(seq 1 100); do
  if DISPLAY="\${DISPLAY}" XAUTHORITY="\${XAUTHORITY}" xdpyinfo >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
DISPLAY="\${DISPLAY}" XAUTHORITY="\${XAUTHORITY}" xdpyinfo >/dev/null 2>&1 || { echo "error: Xvfb failed to start" >&2; exit 1; }

if [[ "${SESSION_MODE}" == "xfce" ]]; then
  "${BIN_DIR}/praxi-xfce-session.sh" &
  SESSION_PID=\$!
  wait -n "\${XVFB_PID}" "\${SESSION_PID}"
else
  wait "\${XVFB_PID}"
fi
EOF
chmod +x "${SESSION_SCRIPT}"

cat > "${DESKTOP_UNIT}" <<EOF
[Unit]
Description=Praxi User-Level Headless X11 Desktop (Xvfb ${DISPLAY_NUM} + ${SESSION_MODE} + AT-SPI2)
After=dbus.socket

[Service]
Type=simple
Environment="DISPLAY=${DISPLAY_NUM}"
Environment="XAUTHORITY=${USER_XAUTH}"
Environment="PRAXI_DBUS_SOCKET=${USER_DBUS}"
Environment="DBUS_SESSION_BUS_ADDRESS=unix:path=${USER_DBUS}"
Environment="XDG_SESSION_TYPE=x11"
Environment="NO_AT_BRIDGE=0"
Environment="GTK_MODULES=gail:atk-bridge"
Environment="QT_ACCESSIBILITY=1"
Environment="QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1"
Environment="GNOME_ACCESSIBILITY=1"
ExecStart=${SESSION_SCRIPT}
Restart=on-failure
RestartSec=2
KillMode=control-group

[Install]
WantedBy=default.target
EOF

if [[ "${NO_START}" -eq 0 ]] && command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
  systemctl --user daemon-reload
  systemctl --user enable --now praxi-headless-desktop.service
  log "systemd user service enabled and started."
else
  log "user desktop unit written (${DESKTOP_UNIT}); start skipped (--no-start or inactive user systemd)."
fi

log "setup complete (mode=${MODE}, session=${SESSION_MODE}, display=${DISPLAY_NUM}, screen=${SCREEN_SPEC})"

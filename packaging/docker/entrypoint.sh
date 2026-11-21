#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="${BOT_DATA_DIR:-/var/lib/praxi}"
export HOME="${HOME:-${DATA_DIR}}"

load_passphrase_file() {
  if [[ -z "${BOT_SECRETS_PASSPHRASE:-}" && -n "${BOT_SECRETS_PASSPHRASE_FILE:-}" ]]; then
    if [[ ! -r "${BOT_SECRETS_PASSPHRASE_FILE}" ]]; then
      echo "praxi-entrypoint: error: BOT_SECRETS_PASSPHRASE_FILE '${BOT_SECRETS_PASSPHRASE_FILE}' is not readable" >&2
      exit 1
    fi
    local pass
    pass="$(<"${BOT_SECRETS_PASSPHRASE_FILE}")"
    pass="${pass%$'\n'}"
    pass="${pass%$'\r'}"
    export BOT_SECRETS_PASSPHRASE="${pass}"
  fi
}

init_xauth() {
  local disp="$1" xauth_file="$2" cookie tmp_auth
  mkdir -p "$(dirname "${xauth_file}")"
  if [[ -s "${xauth_file}" ]] && xauth -f "${xauth_file}" list "${disp}" 2>/dev/null | grep -q 'MIT-MAGIC-COOKIE-1'; then
    chmod 0600 "${xauth_file}"
    return 0
  fi
  if command -v mcookie >/dev/null 2>&1; then
    cookie="$(mcookie)"
  else
    cookie="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
  fi
  tmp_auth="${xauth_file}.tmp.$$"
  umask 077
  : > "${tmp_auth}"
  chmod 0600 "${tmp_auth}"
  xauth -f "${tmp_auth}" add "${disp}" . "${cookie}"
  mv -f "${tmp_auth}" "${xauth_file}"
  chmod 0600 "${xauth_file}"
}

start_xfce_session() {
  local dbus_socket="$1" browser_bin="" candidate
  local xdg_cfg="${XDG_CONFIG_HOME:-${HOME}/.config}"
  local xdg_data="${XDG_DATA_HOME:-${HOME}/.local/share}"
  local xdg_cache="${XDG_CACHE_HOME:-${HOME}/.cache}"

  export XDG_CONFIG_HOME="${xdg_cfg}"
  export XDG_DATA_HOME="${xdg_data}"
  export XDG_CACHE_HOME="${xdg_cache}"
  export DBUS_SESSION_BUS_ADDRESS="unix:path=${dbus_socket}"
  export XDG_SESSION_TYPE="x11"
  export XDG_CURRENT_DESKTOP="XFCE"
  export NO_AT_BRIDGE=0
  export GTK_MODULES="gail:atk-bridge"
  export QT_ACCESSIBILITY=1
  export QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
  export GNOME_ACCESSIBILITY=1

  mkdir -p \
    "${xdg_cfg}/autostart" \
    "${xdg_cfg}/xfce4/xfconf/xfce-perchannel-xml" \
    "${xdg_data}/applications" \
    "${xdg_cache}"

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
    cat > "${xdg_cfg}/autostart/${desktop_name}" <<'AUTOSTART'
[Desktop Entry]
Type=Application
Hidden=true
AUTOSTART
  done

  local xfconf_dir="${xdg_cfg}/xfce4/xfconf/xfce-perchannel-xml"
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

  for candidate in "${BOT_BROWSER_PATH:-}" "${PRAXI_BROWSER_PATH:-}" /opt/praxi-browser/chrome /usr/bin/chromium; do
    [[ -n "${candidate}" && -x "${candidate}" ]] || continue
    browser_bin="${candidate}"
    break
  done
  if [[ -n "${browser_bin}" ]]; then
    export BOT_BROWSER_PATH="${BOT_BROWSER_PATH:-${browser_bin}}"
    cat > "${xdg_data}/applications/praxi-browser.desktop" <<DESKTOP
[Desktop Entry]
Version=1.0
Type=Application
Name=Web Browser
Comment=Open the web browser (opened only by a person, never autostarted)
Exec=${browser_bin} --no-first-run --no-default-browser-check --disable-dev-shm-usage --disable-gpu %U
Icon=web-browser
Terminal=false
Categories=Network;WebBrowser;
StartupNotify=true
DESKTOP
    mkdir -p "${HOME}/Desktop"
    cp "${xdg_data}/applications/praxi-browser.desktop" "${HOME}/Desktop/praxi-browser.desktop"
    chmod 0755 "${HOME}/Desktop/praxi-browser.desktop"
    if [[ ! -f "${xdg_cfg}/xfce4/helpers.rc" ]]; then
      echo "WebBrowser=praxi-browser" > "${xdg_cfg}/xfce4/helpers.rc"
    fi
  fi

  for app_desktop in xfce4-terminal.desktop thunar.desktop; do
    cat > "${xdg_data}/applications/${app_desktop}" <<'APP_HIDE'
[Desktop Entry]
Type=Application
Hidden=true
APP_HIDE
  done

  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${xdg_data}/applications" >/dev/null 2>&1 || true
  fi

  rm -f "${dbus_socket}"
  dbus-daemon --session --address="unix:path=${dbus_socket}" --nofork --nopidfile &
  DBUS_PID=$!

  for _ in $(seq 1 50); do
    if [[ -S "${dbus_socket}" ]]; then
      chmod 0600 "${dbus_socket}"
      break
    fi
    sleep 0.1
  done
  [[ -S "${dbus_socket}" ]] || { echo "praxi-entrypoint: error: session D-Bus socket did not appear" >&2; exit 1; }
  chmod 0600 "${dbus_socket}"

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

  xfwm4 --compositor=off >/dev/null 2>&1 &
  XFWM_PID=$!
  xfdesktop >/dev/null 2>&1 &
  XFDESKTOP_PID=$!
  xfce4-panel >/dev/null 2>&1 &
  XFPANEL_PID=$!
}

XVFB_PID=""
DBUS_PID=""
XFSETTINGSD_PID=""
XFWM_PID=""
XFDESKTOP_PID=""
XFPANEL_PID=""
BOT_PID=""
DBUS_SOCKET=""

# shellcheck disable=SC2329  # invoked via EXIT trap
cleanup_desktop() {
  local pid
  for pid in "${BOT_PID}" "${XFPANEL_PID}" "${XFDESKTOP_PID}" "${XFWM_PID}" "${XFSETTINGSD_PID}" "${DBUS_PID}" "${XVFB_PID}"; do
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      kill -TERM "${pid}" 2>/dev/null || true
    fi
  done
  if [[ -n "${BOT_PID}" ]]; then
    wait "${BOT_PID}" 2>/dev/null || true
  fi
  if [[ -n "${DBUS_SOCKET}" ]]; then
    rm -f "${DBUS_SOCKET}"
  fi
}

# shellcheck disable=SC2329  # invoked via INT/TERM trap
forward_term() {
  if [[ -n "${BOT_PID}" ]] && kill -0 "${BOT_PID}" 2>/dev/null; then
    kill -TERM "${BOT_PID}" 2>/dev/null || true
    wait "${BOT_PID}" || true
  fi
  cleanup_desktop
  exit 0
}

load_passphrase_file

subcmd="${1:-}"
if [[ "${subcmd}" == "serve" && "${BOT_SECRETS_BACKEND:-file}" == "file" && -z "${BOT_SECRETS_PASSPHRASE:-}" ]]; then
  cat >&2 <<'EOF'
praxi-entrypoint: error: BOT_SECRETS_PASSPHRASE is not set.
Headless containers have no OS keychain and require an encrypted file passphrase.
Set BOT_SECRETS_PASSPHRASE in your .env file or mount a secret and set BOT_SECRETS_PASSPHRASE_FILE.
EOF
  exit 1
fi

if [[ "${subcmd}" == "serve" ]] && command -v Xvfb >/dev/null 2>&1 && [[ "${PRAXI_DISABLE_DESKTOP:-0}" != "1" ]]; then
  DISP="${DISPLAY:-:99}"
  SCREEN_SPEC="${PRAXI_SCREEN:-1920x1080x24}"
  XAUTH_FILE="${XAUTHORITY:-${DATA_DIR}/.Xauthority}"
  DBUS_SOCKET="${PRAXI_DBUS_SOCKET:-${DATA_DIR}/.dbus-session}"
  disp_num="${DISP#:}"

  export DISPLAY="${DISP}"
  export XAUTHORITY="${XAUTH_FILE}"

  if [[ ! -d /tmp/.X11-unix ]]; then
    mkdir -p /tmp/.X11-unix
    chmod 1777 /tmp/.X11-unix
  fi
  rm -f "/tmp/.X${disp_num}-lock" "/tmp/.X11-unix/X${disp_num}"

  init_xauth "${DISP}" "${XAUTH_FILE}"

  trap forward_term INT TERM
  trap cleanup_desktop EXIT

  Xvfb "${DISP}" -screen 0 "${SCREEN_SPEC}" -auth "${XAUTH_FILE}" -nolisten tcp +extension RANDR +extension XTEST &
  XVFB_PID=$!

  ready_x11=0
  for _ in $(seq 1 100); do
    if ! kill -0 "${XVFB_PID}" 2>/dev/null; then
      echo "praxi-entrypoint: error: Xvfb exited early" >&2
      exit 1
    fi
    if DISPLAY="${DISP}" XAUTHORITY="${XAUTH_FILE}" xdpyinfo >/dev/null 2>&1; then
      ready_x11=1
      break
    fi
    sleep 0.1
  done
  if [[ "${ready_x11}" -ne 1 ]]; then
    echo "praxi-entrypoint: error: Xvfb on ${DISP} did not become ready within 10s" >&2
    exit 1
  fi

  if command -v xfwm4 >/dev/null 2>&1; then
    start_xfce_session "${DBUS_SOCKET}"
  fi

  /opt/praxi/bin/bot "$@" &
  BOT_PID=$!
  wait "${BOT_PID}"
  rc=$?
  exit "${rc}"
fi

exec /opt/praxi/bin/bot "$@"

#!/usr/bin/env bash
# Install Praxi-Bot as an always-on systemd service on a Linux server
# (x86_64 or aarch64; Debian/Ubuntu or Fedora/RHEL/Oracle Linux family).
#
# Usage (as root):
#   sudo ./install-linux.sh [options]
#   # Script and binaries must come from the same release tag:
#   curl -fsSL https://github.com/emreocell/Praxi-Bot/releases/download/v0.1.0/install-linux.sh | sudo bash -s -- --version v0.1.0
#
# Options:
#   --version <tag>      Release tag to install, e.g. v0.1.0 (default: latest)
#   --file <tarball>     Install from a local praxi-linux-<arch>.tar.gz instead of downloading
#   --sha256 <hex>       Expected SHA-256 of the tarball (default: sibling .sha256, or the release checksum)
#   --insecure-skip-verify
#                        Allow a local --file that has no --sha256 and no sibling .sha256
#   --port <port>        Daemon port on 127.0.0.1 (default: 47821)
#   --with-desktop       Also install Xvfb + Chromium and a praxi-xvfb.service virtual display
#   --without-desktop    Remove an existing praxi-xvfb unit and DISPLAY (updates keep both otherwise)
#   --display <:N>       X display for --with-desktop (default: :99)
#   --screen <WxHxD>     Xvfb screen for --with-desktop (default: 1920x1080x24)
#   --skip-packages      Do not install OS packages (you provide curl, tar, libdbus, libssl, ...)
#   --no-start           Install and enable, but do not start the services
#   --uninstall          Remove the services and /opt/praxi (keeps data and the praxi user)
#   --purge              With --uninstall: also delete /var/lib/praxi, /etc/praxi and the praxi user
#   -h, --help           Show this help

set -euo pipefail

REPO="emreocell/Praxi-Bot"
PREFIX="/opt/praxi"
DATA_DIR="/var/lib/praxi"
CONF_DIR="/etc/praxi"
ENV_FILE="${CONF_DIR}/praxi.env"
NODE_ENV_FILE="${CONF_DIR}/praxi-node.env"
UNIT_DIR="/etc/systemd/system"
SVC_USER="praxi"

VERSION="latest"
LOCAL_FILE=""
EXPECTED_SHA=""
INSECURE_SKIP_VERIFY=0
PORT="47821"
PORT_SET=0
DISPLAY_SET=0
SCREEN_SET=0
WITH_DESKTOP=0
WITHOUT_DESKTOP=0
KEEP_DESKTOP=0
DISPLAY_NUM=":99"
SCREEN_SPEC="1920x1080x24"
SKIP_PACKAGES=0
NO_START=0
UNINSTALL=0
PURGE=0

log() { printf '[praxi-install] %s\n' "$*"; }
warn() { printf '[praxi-install] warning: %s\n' "$*" >&2; }
die() { printf '[praxi-install] error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'USAGE'
Usage (as root):
  sudo ./install-linux.sh [options]
  curl -fsSL https://github.com/emreocell/Praxi-Bot/releases/download/v0.1.0/install-linux.sh | sudo bash -s -- --version v0.1.0

The curl URL tag and --version must be the same release. The release asset is
install-linux.sh (not the copy on the main branch).

Options:
  --version <tag>      Release tag to install, e.g. v0.1.0 (default: latest)
  --file <tarball>     Install from a local praxi-linux-<arch>.tar.gz instead of downloading
  --sha256 <hex>       Expected SHA-256 of the tarball
  --insecure-skip-verify
                       Allow a local --file with no --sha256 and no sibling .sha256
  --port <port>        Daemon port on 127.0.0.1 (default: 47821; kept on re-install unless given)
  --with-desktop       Also install Xvfb + Chromium and a praxi-xvfb.service virtual display
  --without-desktop    Remove praxi-xvfb.service and DISPLAY (an update keeps both unless this is set)
  --display <:N>       X display for --with-desktop (default: :99; kept on re-install unless given)
  --screen <WxHxD>     Xvfb screen for --with-desktop (default: 1920x1080x24)
  --skip-packages      Do not install OS packages (you provide curl, tar, libdbus, libssl, ...)
  --no-start           Install and enable, but do not start the services
  --uninstall          Remove the services and /opt/praxi (keeps data and the praxi user)
  --purge              With --uninstall: also delete /var/lib/praxi, /etc/praxi and the praxi user
  -h, --help           Show this help
USAGE
}

need_arg() { [[ $# -ge 2 && -n "$2" ]] || die "$1 requires a value"; }

# First 64 hex digits, lowercased. Accepts a bare hex string, GNU
# "hex  filename", BSD "SHA256 (file) = hex", and a trailing CR.
sha256_hex() {
  local raw="${1//$'\r'/}"
  raw="${raw,,}"
  if [[ "${raw}" =~ =[[:space:]]*([0-9a-f]{64})[[:space:]]*$ ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
    return
  fi
  if [[ "${raw}" =~ ^[[:space:]]*([0-9a-f]{64})([^0-9a-f]|$) ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
    return
  fi
  printf '%s' "${raw}"
}

# List archive members with tar -tzf and refuse names that could escape the
# temp dir: an absolute path, or any path component that is exactly '..'.
reject_unsafe_tar_members() {
  local archive="$1" member rel part
  local -a parts=()
  tar -tzf "${archive}" > "${WORK}/members.txt" || die "could not list tarball members"
  while IFS= read -r member || [[ -n "${member}" ]]; do
    [[ -n "${member}" ]] || continue
    rel="${member#./}"
    case "${rel}" in
      /*)
        die "tarball member is an absolute path: ${member}"
        ;;
    esac
    IFS='/' read -r -a parts <<< "${rel}"
    for part in "${parts[@]}"; do
      if [[ "${part}" == ".." ]]; then
        die "tarball member contains a '..' component: ${member}"
      fi
    done
  done < "${WORK}/members.txt"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) need_arg "$@"; VERSION="$2"; shift 2 ;;
    --file) need_arg "$@"; LOCAL_FILE="$2"; shift 2 ;;
    --sha256) need_arg "$@"; EXPECTED_SHA="$(sha256_hex "$2")"; shift 2 ;;
    --insecure-skip-verify) INSECURE_SKIP_VERIFY=1; shift ;;
    --port) need_arg "$@"; PORT="$2"; PORT_SET=1; shift 2 ;;
    --with-desktop) WITH_DESKTOP=1; shift ;;
    --without-desktop) WITHOUT_DESKTOP=1; shift ;;
    --display) need_arg "$@"; DISPLAY_NUM="$2"; DISPLAY_SET=1; shift 2 ;;
    --screen) need_arg "$@"; SCREEN_SPEC="$2"; SCREEN_SET=1; shift 2 ;;
    --skip-packages) SKIP_PACKAGES=1; shift ;;
    --no-start) NO_START=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    --purge) PURGE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument '$1'" ;;
  esac
done

[[ "$(uname -s)" == "Linux" ]] || die "this script only supports Linux"
[[ "${EUID:-$(id -u)}" -eq 0 ]] || die "run as root (use sudo)"
command -v systemctl >/dev/null 2>&1 || die "systemd is required"
[[ "${PORT}" =~ ^[0-9]+$ && "${PORT}" -ge 1 && "${PORT}" -le 65535 ]] || die "invalid --port"
[[ "${DISPLAY_NUM}" =~ ^:[0-9]+$ ]] || die "invalid --display (expected :N)"
[[ "${SCREEN_SPEC}" =~ ^[0-9]+x[0-9]+x[0-9]+$ ]] || die "invalid --screen (expected WxHxD)"
[[ "${PURGE}" -eq 0 || "${UNINSTALL}" -eq 1 ]] || die "--purge only makes sense with --uninstall"
[[ "${WITH_DESKTOP}" -eq 0 || "${WITHOUT_DESKTOP}" -eq 0 ]] || die "--with-desktop and --without-desktop cannot be used together"
[[ "${WITHOUT_DESKTOP}" -eq 0 || "${DISPLAY_SET}" -eq 0 ]] || die "--display cannot be combined with --without-desktop"
[[ "${WITHOUT_DESKTOP}" -eq 0 || "${SCREEN_SET}" -eq 0 ]] || die "--screen cannot be combined with --without-desktop"

# ---------------------------------------------------------------- uninstall
if [[ "${UNINSTALL}" -eq 1 ]]; then
  log "stopping and removing services"
  for u in praxi.service praxi-node.service praxi-xvfb.service; do
    systemctl disable --now "${u}" >/dev/null 2>&1 || true
    rm -f "${UNIT_DIR:?}/${u:?}"
  done
  rm -rf "${UNIT_DIR:?}/praxi.service.d" "${UNIT_DIR:?}/praxi-node.service.d"
  systemctl daemon-reload
  rm -rf "${PREFIX:?}"
  rm -f /usr/local/bin/praxi /usr/local/bin/praxi-node
  if [[ "${PURGE}" -eq 1 ]]; then
    log "purging data (${DATA_DIR}), config (${CONF_DIR}) and the ${SVC_USER} user"
    rm -rf "${DATA_DIR:?}" "${CONF_DIR:?}"
    if id -u "${SVC_USER}" >/dev/null 2>&1; then userdel "${SVC_USER}" || true; fi
    if getent group "${SVC_USER}" >/dev/null 2>&1; then groupdel "${SVC_USER}" || true; fi
  else
    log "kept ${DATA_DIR}, ${CONF_DIR} and the ${SVC_USER} user (use --purge to delete them)"
  fi
  log "uninstalled"
  exit 0
fi

# Updates keep an existing virtual display. --without-desktop is the only
# switch that removes praxi-xvfb.service and DISPLAY.
if [[ "${WITH_DESKTOP}" -eq 0 && "${WITHOUT_DESKTOP}" -eq 0 && -f "${UNIT_DIR}/praxi-xvfb.service" ]]; then
  KEEP_DESKTOP=1
  log "keeping existing praxi-xvfb.service and DISPLAY (pass --without-desktop to remove desktop mode)"
fi

# ------------------------------------------------------------------- arch
case "$(uname -m)" in
  x86_64|amd64) ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) die "unsupported architecture '$(uname -m)' (need x86_64 or aarch64)" ;;
esac
log "architecture: ${ARCH}"
ASSET="praxi-linux-${ARCH}.tar.gz"

# --------------------------------------------------------------- packages
install_packages() {
  local pkgs_apt=(curl ca-certificates tar libdbus-1-3)
  # bot and bot-node link libssl.so.3 (native-tls via iroh/rmcp). curl usually
  # pulls it in; openssl-libs is the dnf name. The apt name is chosen below.
  local pkgs_dnf=(curl ca-certificates tar dbus-libs openssl-libs)
  if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
    pkgs_apt+=(xvfb at-spi2-core fonts-liberation fonts-noto-core)
    pkgs_dnf+=(xorg-x11-server-Xvfb at-spi2-core liberation-fonts)
  fi
  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -y
    apt-get install -y --no-install-recommends "${pkgs_apt[@]}"
    # Debian 12 / Ubuntu: libssl3. Debian 13 renamed it to libssl3t64.
    if apt-cache show libssl3 >/dev/null 2>&1; then
      apt-get install -y --no-install-recommends libssl3
    elif apt-cache show libssl3t64 >/dev/null 2>&1; then
      apt-get install -y --no-install-recommends libssl3t64
    else
      warn "no libssl3 package found; the binaries need libssl.so.3"
    fi
    if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
      # Ubuntu's chromium-browser is a snap stub; snap confinement does not
      # work well under a hardened system service. Prefer a real deb.
      if apt-cache show chromium >/dev/null 2>&1; then
        apt-get install -y --no-install-recommends chromium
      elif apt-cache show chromium-browser >/dev/null 2>&1; then
        apt-get install -y --no-install-recommends chromium-browser
        warn "chromium-browser may be a snap on this distro; if browser tools fail, install a non-snap Chromium (see docs/guide/remote-host.md)"
      else
        warn "no chromium package found; install a Chromium-family browser yourself"
      fi
    fi
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y "${pkgs_dnf[@]}"
    if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
      dnf install -y chromium || warn "chromium is not in your enabled repos (on Oracle Linux enable EPEL); install a Chromium-family browser yourself"
    fi
  else
    warn "unknown package manager; install curl, tar, libdbus, libssl.so.3 (and Xvfb + Chromium for --with-desktop) yourself"
  fi
}

if [[ "${SKIP_PACKAGES}" -eq 0 ]]; then
  log "installing OS packages"
  install_packages
fi

# --------------------------------------------------------------- download
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK:?}"' EXIT

if [[ -n "${LOCAL_FILE}" ]]; then
  [[ -f "${LOCAL_FILE}" ]] || die "file not found: ${LOCAL_FILE}"
  cp "${LOCAL_FILE}" "${WORK}/${ASSET}"
  if [[ -z "${EXPECTED_SHA}" && -f "${LOCAL_FILE}.sha256" ]]; then
    EXPECTED_SHA="$(sha256_hex "$(awk 'NR==1 { print; exit }' "${LOCAL_FILE}.sha256")")"
  fi
  if [[ -z "${EXPECTED_SHA}" && "${INSECURE_SKIP_VERIFY}" -eq 0 ]]; then
    die "no SHA-256 for local file ${LOCAL_FILE} (pass --sha256, provide ${LOCAL_FILE}.sha256, or pass --insecure-skip-verify)"
  fi
else
  if [[ "${VERSION}" == "latest" ]]; then
    BASE="https://github.com/${REPO}/releases/latest/download"
  else
    BASE="https://github.com/${REPO}/releases/download/${VERSION}"
  fi
  log "downloading ${BASE}/${ASSET}"
  curl -fSL --proto '=https' --tlsv1.2 -o "${WORK}/${ASSET}" "${BASE}/${ASSET}" \
    || die "download failed (no release published yet? build it with the 'Linux Server Packages' workflow and pass --file)"
  if [[ -z "${EXPECTED_SHA}" ]]; then
    curl -fSL --proto '=https' --tlsv1.2 -o "${WORK}/${ASSET}.sha256" "${BASE}/${ASSET}.sha256" \
      || die "could not fetch the checksum file"
    EXPECTED_SHA="$(sha256_hex "$(awk 'NR==1 { print; exit }' "${WORK}/${ASSET}.sha256")")"
  fi
fi

if [[ -n "${EXPECTED_SHA}" ]]; then
  ACTUAL_SHA="$(sha256_hex "$(sha256sum "${WORK}/${ASSET}")")"
  [[ "${ACTUAL_SHA}" == "${EXPECTED_SHA}" ]] || die "SHA-256 mismatch (expected ${EXPECTED_SHA}, got ${ACTUAL_SHA})"
  log "checksum OK"
elif [[ "${INSECURE_SKIP_VERIFY}" -eq 1 ]]; then
  warn "checksum verification skipped (--insecure-skip-verify)"
else
  die "no SHA-256 available to verify the tarball"
fi

# List members before extracting as root. Absolute paths and '..' components
# are rejected; the archive is extracted only into the temp dir.
reject_unsafe_tar_members "${WORK}/${ASSET}"
mkdir "${WORK}/x"
tar -xzf "${WORK}/${ASSET}" -C "${WORK}/x"
SRC="${WORK}/x/praxi-linux-${ARCH}"
[[ -x "${SRC}/bin/bot" && -x "${SRC}/bin/bot-node" ]] || die "tarball does not contain bin/bot and bin/bot-node"

# ------------------------------------------------------------------ user
if ! id -u "${SVC_USER}" >/dev/null 2>&1; then
  log "creating system user ${SVC_USER}"
  useradd --system --user-group --home-dir "${DATA_DIR}" --no-create-home \
    --shell /usr/sbin/nologin "${SVC_USER}"
fi
install -d -m 0750 -o "${SVC_USER}" -g "${SVC_USER}" "${DATA_DIR}"
install -d -m 0750 -o root -g "${SVC_USER}" "${CONF_DIR}"

# ----------------------------------------------------------------- binaries
NODE_WAS_ACTIVE=0
if systemctl is-active --quiet praxi-node.service 2>/dev/null; then
  NODE_WAS_ACTIVE=1
  systemctl stop praxi-node.service
fi
if systemctl is-active --quiet praxi.service 2>/dev/null; then
  systemctl stop praxi.service
fi
install -d -m 0755 "${PREFIX}/bin"
install -m 0755 "${SRC}/bin/bot" "${PREFIX}/bin/bot"
install -m 0755 "${SRC}/bin/bot-node" "${PREFIX}/bin/bot-node"
for f in VERSION LICENSE NOTICE THIRD_PARTY_NOTICES.md; do
  if [[ -f "${SRC}/${f}" ]]; then install -m 0644 "${SRC}/${f}" "${PREFIX}/${f}"; fi
done
log "installed to ${PREFIX} ($(cat "${PREFIX}/VERSION" 2>/dev/null || echo unknown version))"

# Wrapper commands: run a binary as the service user with that service's env file.
# praxi-node must source praxi-node.env so it does not receive the daemon passphrase.
write_wrapper() { # <command name> <binary name> <env file>
  local cmd="$1" bin="$2" envf="$3"
  cat > "/usr/local/bin/${cmd}" <<WRAP
#!/bin/sh
# Runs ${PREFIX}/bin/${bin} as the ${SVC_USER} user with ${envf}.
inner='set -a; . ${envf}; set +a; exec "\$@"'
if [ "\$(id -u)" -eq 0 ]; then
  exec runuser -u ${SVC_USER} -- sh -c "\$inner" sh ${PREFIX}/bin/${bin} "\$@"
fi
exec sudo -u ${SVC_USER} sh -c "\$inner" sh ${PREFIX}/bin/${bin} "\$@"
WRAP
  chmod 0755 "/usr/local/bin/${cmd}"
}
write_wrapper praxi bot "${ENV_FILE}"
write_wrapper praxi-node bot-node "${NODE_ENV_FILE}"

# ------------------------------------------------------------ environment
if [[ ! -f "${ENV_FILE}" ]]; then
  log "creating ${ENV_FILE}"
  # The headless secret store is an encrypted file; its passphrase lives here.
  PASSPHRASE="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  (
    umask 077
    {
      echo "BOT_DATA_DIR=${DATA_DIR}"
      echo "BOT_PORT=${PORT}"
      echo "BOT_SECRETS_BACKEND=file"
      echo "BOT_SECRETS_PASSPHRASE=${PASSPHRASE}"
      echo "HOME=${DATA_DIR}"
    } > "${ENV_FILE}"
  )
else
  log "keeping existing ${ENV_FILE}"
  if [[ "${PORT_SET}" -eq 1 ]]; then
    sed -i '/^BOT_PORT=/d' "${ENV_FILE}"
    echo "BOT_PORT=${PORT}" >> "${ENV_FILE}"
  elif grep -q '^BOT_PORT=' "${ENV_FILE}"; then
    PORT="$(grep '^BOT_PORT=' "${ENV_FILE}" | tail -n1 | cut -d= -f2- | tr -d '[:space:]')"
    [[ "${PORT}" =~ ^[0-9]+$ && "${PORT}" -ge 1 && "${PORT}" -le 65535 ]] || die "invalid BOT_PORT in ${ENV_FILE}"
  else
    echo "BOT_PORT=${PORT}" >> "${ENV_FILE}"
  fi
fi
if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
  if [[ "${DISPLAY_SET}" -eq 0 ]] && grep -q '^DISPLAY=:[0-9][0-9]*$' "${ENV_FILE}"; then
    DISPLAY_NUM="$(grep '^DISPLAY=' "${ENV_FILE}" | tail -n1 | cut -d= -f2-)"
  else
    sed -i '/^DISPLAY=/d' "${ENV_FILE}"
    echo "DISPLAY=${DISPLAY_NUM}" >> "${ENV_FILE}"
  fi
elif [[ "${KEEP_DESKTOP}" -eq 1 ]]; then
  # Leave an existing DISPLAY in place unless --display was passed.
  if [[ "${DISPLAY_SET}" -eq 1 ]]; then
    sed -i '/^DISPLAY=/d' "${ENV_FILE}"
    echo "DISPLAY=${DISPLAY_NUM}" >> "${ENV_FILE}"
  elif grep -q '^DISPLAY=:[0-9][0-9]*$' "${ENV_FILE}"; then
    DISPLAY_NUM="$(grep '^DISPLAY=' "${ENV_FILE}" | tail -n1 | cut -d= -f2-)"
  fi
elif [[ "${WITHOUT_DESKTOP}" -eq 1 ]]; then
  sed -i '/^DISPLAY=/d' "${ENV_FILE}"
fi

# Node-agent environment. Rewritten every run so it cannot carry
# BOT_SECRETS_PASSPHRASE (that stays in praxi.env for the daemon only).
log "writing ${NODE_ENV_FILE}"
(
  umask 077
  {
    echo "BOT_DATA_DIR=${DATA_DIR}"
    echo "HOME=${DATA_DIR}"
    if [[ "${WITHOUT_DESKTOP}" -eq 0 ]] && grep -q '^DISPLAY=:[0-9][0-9]*$' "${ENV_FILE}"; then
      grep '^DISPLAY=' "${ENV_FILE}" | tail -n1
    fi
  } > "${NODE_ENV_FILE}"
)
if grep -q '^BOT_SECRETS_PASSPHRASE=' "${NODE_ENV_FILE}"; then
  die "refusing to leave BOT_SECRETS_PASSPHRASE in ${NODE_ENV_FILE}"
fi
chown root:"${SVC_USER}" "${ENV_FILE}" "${NODE_ENV_FILE}"
chmod 0640 "${ENV_FILE}" "${NODE_ENV_FILE}"

# ------------------------------------------------------------------ units
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
UMask=0077"
# Not set on purpose: MemoryDenyWriteExecute and RestrictNamespaces (Chromium needs them).

cat > "${UNIT_DIR}/praxi.service" <<UNIT
[Unit]
Description=Praxi-Bot daemon (bot serve, 127.0.0.1 only)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${SVC_USER}
Group=${SVC_USER}
WorkingDirectory=${DATA_DIR}
EnvironmentFile=${ENV_FILE}
ExecStart=${PREFIX}/bin/bot serve
Restart=on-failure
RestartSec=5
TimeoutStopSec=30
${HARDENING}

[Install]
WantedBy=multi-user.target
UNIT

# Node-agent: installed but NOT enabled; pair it first (praxi-node join <ticket>).
cat > "${UNIT_DIR}/praxi-node.service" <<UNIT
[Unit]
Description=Praxi node-agent (bot-node run; dial-out only, opens no port)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${SVC_USER}
Group=${SVC_USER}
WorkingDirectory=${DATA_DIR}
EnvironmentFile=${NODE_ENV_FILE}
ExecStart=${PREFIX}/bin/bot-node run
Restart=on-failure
RestartSec=10
${HARDENING}

[Install]
WantedBy=multi-user.target
UNIT

# Only desktop.conf belongs to the installer. Other drop-ins (for example a
# Chromium relaxation; see docs/guide/remote-host.md) are left in place.
rm -f "${UNIT_DIR}/praxi.service.d/desktop.conf" "${UNIT_DIR}/praxi-node.service.d/desktop.conf"

write_desktop_dropins() {
  local svc
  for svc in praxi praxi-node; do
    install -d "${UNIT_DIR}/${svc}.service.d"
    # Share the private /tmp (X11 socket) with the display service.
    cat > "${UNIT_DIR}/${svc}.service.d/desktop.conf" <<UNIT
[Unit]
Requires=praxi-xvfb.service
After=praxi-xvfb.service
JoinsNamespaceOf=praxi-xvfb.service
UNIT
  done
}

write_xvfb_unit() {
  local xvfb_bin
  xvfb_bin="$(command -v Xvfb || true)"
  [[ -n "${xvfb_bin}" ]] || die "Xvfb not found (install it or drop --skip-packages)"
  cat > "${UNIT_DIR}/praxi-xvfb.service" <<UNIT
[Unit]
Description=Praxi virtual display (Xvfb ${DISPLAY_NUM})

[Service]
Type=simple
User=${SVC_USER}
Group=${SVC_USER}
ExecStart=${xvfb_bin} ${DISPLAY_NUM} -screen 0 ${SCREEN_SPEC} -nolisten tcp +extension RANDR +extension XTEST
Restart=on-failure
RestartSec=3
${HARDENING}

[Install]
WantedBy=multi-user.target
UNIT
}

if [[ "${WITHOUT_DESKTOP}" -eq 1 && -f "${UNIT_DIR}/praxi-xvfb.service" ]]; then
  log "removing praxi-xvfb.service and DISPLAY (--without-desktop)"
  systemctl disable --now praxi-xvfb.service >/dev/null 2>&1 || true
  rm -f "${UNIT_DIR}/praxi-xvfb.service"
fi
if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
  write_xvfb_unit
  write_desktop_dropins
elif [[ "${KEEP_DESKTOP}" -eq 1 ]]; then
  if [[ "${DISPLAY_SET}" -eq 1 || "${SCREEN_SET}" -eq 1 ]]; then
    if [[ -f "${UNIT_DIR}/praxi-xvfb.service" ]]; then
      xvfb_line="$(grep '^ExecStart=' "${UNIT_DIR}/praxi-xvfb.service" | tail -n1 || true)"
      if [[ "${SCREEN_SET}" -eq 0 && "${xvfb_line}" =~ -screen[[:space:]]+0[[:space:]]+([0-9]+x[0-9]+x[0-9]+) ]]; then
        SCREEN_SPEC="${BASH_REMATCH[1]}"
      fi
      # DISPLAY in the env file wins. If it was never recorded, keep the
      # display number already on the unit and copy it into both env files.
      if [[ "${DISPLAY_SET}" -eq 0 ]] && ! grep -q '^DISPLAY=:[0-9][0-9]*$' "${ENV_FILE}"; then
        if [[ "${xvfb_line}" =~ (^|[[:space:]])(:[0-9]+)([[:space:]]|$) ]]; then
          DISPLAY_NUM="${BASH_REMATCH[2]}"
          echo "DISPLAY=${DISPLAY_NUM}" >> "${ENV_FILE}"
          echo "DISPLAY=${DISPLAY_NUM}" >> "${NODE_ENV_FILE}"
        fi
      fi
    fi
    write_xvfb_unit
  fi
  write_desktop_dropins
fi

systemctl daemon-reload
systemctl enable praxi.service >/dev/null
if [[ "${WITH_DESKTOP}" -eq 1 || "${KEEP_DESKTOP}" -eq 1 ]]; then
  systemctl enable praxi-xvfb.service >/dev/null
fi

if [[ "${NO_START}" -eq 0 ]]; then
  # restart returns non-zero when the start job fails. Under `set -e` that
  # would abort before the warning and before praxi-node is brought back.
  if systemctl restart praxi.service; then
    sleep 2
    if systemctl is-active --quiet praxi.service; then
      log "praxi.service is running"
    else
      warn "praxi.service did not stay up; see: journalctl -u praxi -n 50"
    fi
  else
    warn "praxi.service did not start; see: journalctl -u praxi -n 50"
  fi
  if [[ "${NODE_WAS_ACTIVE}" -eq 1 ]]; then
    systemctl restart praxi-node.service || warn "praxi-node.service did not restart; see: journalctl -u praxi-node -n 50"
  fi
fi

# ------------------------------------------------------------ next steps
cat <<EOF

Praxi-Bot is installed.

  Daemon:   praxi.service, listening on 127.0.0.1:${PORT} only (never expose this port)
  Data:     ${DATA_DIR}
  Env:      ${ENV_FILE} (mode 0640; daemon, contains the secret-store passphrase)
            ${NODE_ENV_FILE} (mode 0640; node-agent, no BOT_SECRETS_PASSPHRASE)
  Logs:     journalctl -u praxi -f
  CLI:      praxi <command>        (wrapper for 'bot' running as the ${SVC_USER} user)

Next steps:
  1. Configure a provider and your first bot:   sudo praxi setup
  2. Check the installation:                     sudo praxi doctor
  3. Remote access: use an SSH tunnel, do not open the port:
       ssh -L ${PORT}:127.0.0.1:${PORT} user@<this-host>
     or pair a Telegram account:                 sudo praxi telegram pair
  4. Controlling this machine from another core is not functional on Linux yet.
     bot-node has no Linux desktop backend, so 'bot-node run' exits and
     praxi-node.service would restart every 10s. Leave that unit disabled.
     Pairing, once the backend ships:
       sudo praxi-node join '<bot-pair:...ticket>'
  5. Update later by re-running this script. An existing praxi-xvfb unit and
     DISPLAY stay in place; pass --without-desktop to remove them.
     Remove the install with: install-linux.sh --uninstall

See docs/guide/remote-host.md for firewall notes (Oracle Cloud) and security guidance.
EOF

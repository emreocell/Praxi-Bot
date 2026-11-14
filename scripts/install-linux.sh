#!/usr/bin/env bash
# Install Praxi-Bot as an always-on systemd service on a Linux server
# (x86_64 or aarch64; Debian/Ubuntu or Fedora/RHEL/Oracle Linux family).
#
# Trust: use independently installed GitHub CLI >= 2.80.0 (https://cli.github.com).
# Verify the installer itself BEFORE running it, including with sudo. Copy this
# bootstrap from a reviewed copy of this repository; replace the example tag:
#   (
#     set -euo pipefail
#     version=v0.1.0
#     work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
#     base="https://github.com/emreocell/Praxi-Bot/releases/download/$version"
#     for asset in install-linux.sh praxi-linux.sigstore.json; do
#       curl -fSL --proto '=https' --proto-redir '=https' --tlsv1.2 "$base/$asset" -o "$work/$asset"
#     done
#     gh attestation verify "$work/install-linux.sh" --bundle "$work/praxi-linux.sigstore.json" \
#       --repo emreocell/Praxi-Bot --hostname github.com \
#       --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
#       --source-ref "refs/tags/$version" --deny-self-hosted-runners \
#       --cert-oidc-issuer https://token.actions.githubusercontent.com \
#       --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256
#     sudo bash "$work/install-linux.sh" --version "$version"
#   )
#
# Options:
#   --version <tag>      Release tag to install, e.g. v0.1.0 (default: latest)
#   --file <tarball>     Use a local archive (requires an explicit --version)
#   --bundle <file>      Local provenance bundle (default: sibling praxi-linux.sigstore.json)
#   --custom-trusted-root <file>  Independently obtained Sigstore trusted_root.jsonl
#   --offline           Require local archive, bundle, root, and --skip-packages
#   --sha256 <hex>       Additional checksum check; never replaces provenance verification
#   --verify-only       Verify without installing; does not require root or Linux
#   --port <port>        Daemon port on 127.0.0.1 (default: 47821)
#   --with-desktop       Also install Xvfb + Chromium and a praxi-xvfb.service virtual display
#   --with-desktop-session  Also install minimal Xfce session on Xvfb (default 1280x800x24; implies --with-desktop)
#   --without-desktop-session  Remove praxi-desktop.service and session bus env (keeps praxi-xvfb unless --without-desktop)
#   --desktop-session <none|xfce>  Explicit desktop session selector (xfce or none)
#   --without-desktop    Remove praxi-xvfb.service, praxi-desktop.service and DISPLAY (updates keep both otherwise)
#   --display <:N>       X display for --with-desktop (default: :99)
#   --screen <WxHxD>     Xvfb screen (default: 1920x1080x24, or 1280x800x24 with --with-desktop-session)
#   --skip-packages      Do not install OS packages (you provide curl, tar, libdbus, libssl, ...)
#   --no-start           Install and enable, but do not start the services
#   --uninstall          Remove the services and /opt/praxi (keeps data and the praxi user)
#   --purge              With --uninstall: also delete /var/lib/praxi, /etc/praxi and the praxi user
#   -h, --help           Show this help

set -euo pipefail

readonly REPO="emreocell/Praxi-Bot"
readonly SIGNER_WORKFLOW="github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml"
readonly BUNDLE_ASSET="praxi-linux.sigstore.json"
readonly MIN_GH_VERSION="2.80.0"
PREFIX="/opt/praxi"
LIBEXEC_DIR="${PREFIX}/libexec"
DATA_DIR="/var/lib/praxi"
CONF_DIR="/etc/praxi"
ENV_FILE="${CONF_DIR}/praxi.env"
NODE_ENV_FILE="${CONF_DIR}/praxi-node.env"
UNIT_DIR="/etc/systemd/system"
SLICE_NAME="praxi.slice"
SVC_USER="praxi"

VERSION="latest"
LOCAL_FILE=""
LOCAL_BUNDLE=""
TRUSTED_ROOT=""
OFFLINE=0
EXPECTED_SHA=""
VERIFY_ONLY=0
PORT="47821"
PORT_SET=0
DISPLAY_SET=0
SCREEN_SET=0
WITH_DESKTOP=0
WITHOUT_DESKTOP=0
KEEP_DESKTOP=0
WITH_DESKTOP_SESSION=0
WITHOUT_DESKTOP_SESSION=0
KEEP_DESKTOP_SESSION=0
DESKTOP_SESSION_ARG=""
DISPLAY_NUM=":99"
SCREEN_SPEC="1920x1080x24"
XAUTH_FILE="${DATA_DIR}/.Xauthority"
DBUS_SOCKET="${DATA_DIR}/.dbus-session"
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

Install trusted GitHub CLI >= 2.80.0 independently (https://cli.github.com).
This is the supported floor for all required identity/offline flags. No verifier is
downloaded or installed by this script. Old releases without provenance fail.

Verified bootstrap (copy from a reviewed repository, choose a release tag):
  (
    set -euo pipefail
    version=v0.1.0
    work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
    base="https://github.com/emreocell/Praxi-Bot/releases/download/$version"
    for asset in install-linux.sh praxi-linux.sigstore.json; do
      curl -fSL --proto '=https' --proto-redir '=https' --tlsv1.2 "$base/$asset" -o "$work/$asset"
    done
    gh attestation verify "$work/install-linux.sh" --bundle "$work/praxi-linux.sigstore.json" \
      --repo emreocell/Praxi-Bot --hostname github.com \
      --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
      --source-ref "refs/tags/$version" --deny-self-hosted-runners \
      --cert-oidc-issuer https://token.actions.githubusercontent.com \
      --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256
    sudo bash "$work/install-linux.sh" --version "$version"
  )

Both the installer and archive must be attested by the pinned release workflow
for the chosen tag. Checksums alone never authorize installation. `latest`
selects the channel's current tag; choose an explicit tag to pin the version.

Options:
  --version <tag>      Release tag to install, e.g. v0.1.0 (default: latest)
  --file <tarball>     Use a local archive (requires an explicit --version)
  --bundle <file>      Local provenance bundle (default: sibling praxi-linux.sigstore.json)
  --custom-trusted-root <file>  Independently obtained Sigstore trusted_root.jsonl
  --offline           Require local archive, bundle and root; no network downloads
  --sha256 <hex>       Additional checksum check; never replaces provenance verification
  --verify-only       Verify without installing; does not require root or Linux
  --port <port>        Daemon port on 127.0.0.1 (default: 47821; kept on re-install unless given)
  --with-desktop       Also install Xvfb + Chromium and a praxi-xvfb.service virtual display
  --with-desktop-session  Also install minimal Xfce session on Xvfb (default 1280x800x24; implies --with-desktop)
  --without-desktop-session  Remove praxi-desktop.service and session bus env (keeps praxi-xvfb unless --without-desktop)
  --desktop-session <none|xfce>  Explicit desktop session selector (xfce or none)
  --without-desktop    Remove praxi-xvfb.service, praxi-desktop.service and DISPLAY (updates keep both otherwise)
  --display <:N>       X display for --with-desktop (default: :99; kept on re-install unless given)
  --screen <WxHxD>     Xvfb screen (default: 1920x1080x24, or 1280x800x24 with --with-desktop-session)
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
    --bundle) need_arg "$@"; LOCAL_BUNDLE="$2"; shift 2 ;;
    --custom-trusted-root) need_arg "$@"; TRUSTED_ROOT="$2"; shift 2 ;;
    --offline) OFFLINE=1; shift ;;
    --sha256) need_arg "$@"; EXPECTED_SHA="$(sha256_hex "$2")"; shift 2 ;;
    --verify-only) VERIFY_ONLY=1; shift ;;
    --port) need_arg "$@"; PORT="$2"; PORT_SET=1; shift 2 ;;
    --with-desktop) WITH_DESKTOP=1; shift ;;
    --with-desktop-session) WITH_DESKTOP_SESSION=1; WITH_DESKTOP=1; shift ;;
    --without-desktop-session) WITHOUT_DESKTOP_SESSION=1; shift ;;
    --desktop-session)
      need_arg "$@"
      DESKTOP_SESSION_ARG="$2"
      case "${DESKTOP_SESSION_ARG}" in
        xfce) WITH_DESKTOP_SESSION=1; WITH_DESKTOP=1 ;;
        none) WITHOUT_DESKTOP_SESSION=1 ;;
        *) die "invalid --desktop-session '${DESKTOP_SESSION_ARG}' (expected xfce or none)" ;;
      esac
      shift 2
      ;;
    --without-desktop) WITHOUT_DESKTOP=1; WITHOUT_DESKTOP_SESSION=1; shift ;;
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

if [[ "${VERIFY_ONLY}" -eq 0 ]]; then
  [[ "$(uname -s)" == "Linux" ]] || die "installation only supports Linux (use --verify-only elsewhere)"
  [[ "${EUID:-$(id -u)}" -eq 0 ]] || die "run as root (use sudo), or pass --verify-only"
  command -v systemctl >/dev/null 2>&1 || die "systemd is required"
fi
[[ "${VERIFY_ONLY}" -eq 0 || "${UNINSTALL}" -eq 0 ]] || die "--verify-only cannot be combined with --uninstall"
[[ "${PORT}" =~ ^[0-9]+$ && "${PORT}" -ge 1 && "${PORT}" -le 65535 ]] || die "invalid --port"
[[ "${DISPLAY_NUM}" =~ ^:[0-9]+$ ]] || die "invalid --display (expected :N)"
[[ "${SCREEN_SPEC}" =~ ^[0-9]+x[0-9]+x[0-9]+$ ]] || die "invalid --screen (expected WxHxD)"
[[ "${PURGE}" -eq 0 || "${UNINSTALL}" -eq 1 ]] || die "--purge only makes sense with --uninstall"
[[ "${WITH_DESKTOP}" -eq 0 || "${WITHOUT_DESKTOP}" -eq 0 ]] || die "--with-desktop/--with-desktop-session and --without-desktop cannot be used together"
[[ "${WITH_DESKTOP_SESSION}" -eq 0 || "${WITHOUT_DESKTOP_SESSION}" -eq 0 ]] || die "--with-desktop-session and --without-desktop-session cannot be used together"
[[ "${WITHOUT_DESKTOP}" -eq 0 || "${DISPLAY_SET}" -eq 0 ]] || die "--display cannot be combined with --without-desktop"
[[ "${WITHOUT_DESKTOP}" -eq 0 || "${SCREEN_SET}" -eq 0 ]] || die "--screen cannot be combined with --without-desktop"

# ---------------------------------------------------------------- uninstall
if [[ "${UNINSTALL}" -eq 1 ]]; then
  log "stopping and removing services"
  for u in praxi.service praxi-node.service praxi-desktop.service praxi-xvfb.service "${SLICE_NAME}"; do
    systemctl disable --now "${u}" >/dev/null 2>&1 || true
    rm -f "${UNIT_DIR:?}/${u:?}"
  done
  rm -rf "${UNIT_DIR:?}/praxi.service.d" "${UNIT_DIR:?}/praxi-node.service.d" \
    "${UNIT_DIR:?}/praxi-desktop.service.d" "${UNIT_DIR:?}/praxi-xvfb.service.d"
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

# Updates keep an existing virtual display and desktop session unless explicitly
# disabled. A bare-Xvfb host keeps bare Xvfb unless --with-desktop-session is passed.
if [[ "${WITH_DESKTOP}" -eq 0 && "${WITHOUT_DESKTOP}" -eq 0 && -f "${UNIT_DIR}/praxi-xvfb.service" ]]; then
  KEEP_DESKTOP=1
  log "keeping existing praxi-xvfb.service and DISPLAY (pass --without-desktop to remove desktop mode)"
fi
if [[ "${WITH_DESKTOP_SESSION}" -eq 0 && "${WITHOUT_DESKTOP_SESSION}" -eq 0 && -f "${UNIT_DIR}/praxi-desktop.service" ]]; then
  KEEP_DESKTOP_SESSION=1
  KEEP_DESKTOP=1
  log "keeping existing praxi-desktop.service (pass --without-desktop-session to remove desktop session)"
fi
if [[ "${WITH_DESKTOP_SESSION}" -eq 1 && "${SCREEN_SET}" -eq 0 && "${KEEP_DESKTOP_SESSION}" -eq 0 && ! -f "${UNIT_DIR}/praxi-desktop.service" ]]; then
  SCREEN_SPEC="1280x800x24"
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
# Test actual candidate data, not apt-cache's success status on empty output.
apt_candidate() {
  local candidate
  candidate="$(apt-cache policy "$1" | awk '/Candidate:/ {print $2}')"
  [[ -n "${candidate}" && "${candidate}" != "(none)" && "${candidate}" != *snap* ]]
}

install_cft() {
  # Official Google Chrome for Testing, pinned, for both supported CPUs.
  # No snap, PPA, Node runtime or printer services. Update the pin with releases.
  local version="157.0.8103.38"
  local sha256_linux64="8cdeb344e9b2cefe59f4aea8d1d93d5d93054c03cde217665c16fc1f62e72a66"
  local sha256_linux_arm64="5517510e61657ff43ee9ab804ecd1a499b3d5bd3caf72f097516bd6565ba5a17"
  local platform=linux64 expected_sha="${sha256_linux64}" actual_sha browser_work pkg
  if [[ "${ARCH}" == "aarch64" ]]; then
    platform=linux-arm64
    expected_sha="${sha256_linux_arm64}"
  fi
  local libs=(libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2 libxkbcommon0
    libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libpango-1.0-0 libcairo2)
  for pkg in libasound2 libatspi2.0-0; do
    if apt_candidate "${pkg}t64"; then libs+=("${pkg}t64"); else libs+=("${pkg}"); fi
  done
  apt-get install -y --no-install-recommends unzip "${libs[@]}"
  browser_work="$(mktemp -d)"
  log "installing official Chrome for Testing ${version} (${platform}); no snap/PPA"
  if ! curl -fSL --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 180 \
    "https://storage.googleapis.com/chrome-for-testing-public/${version}/${platform}/chrome-${platform}.zip" \
    -o "${browser_work}/chrome.zip"; then
    rm -rf "${browser_work}"
    die "Chrome for Testing download failed; install a non-snap browser and use --skip-packages"
  fi
  actual_sha="$(sha256_hex "$(sha256sum "${browser_work}/chrome.zip")")"
  if [[ "${actual_sha}" != "${expected_sha}" ]]; then
    rm -f "${browser_work}/chrome.zip"
    rm -rf "${browser_work}"
    die "Chrome for Testing SHA-256 mismatch for ${platform} (expected ${expected_sha}, got ${actual_sha})"
  fi
  log "Chrome for Testing checksum OK"
  unzip -q "${browser_work}/chrome.zip" -d "${browser_work}"
  install -d -m 0755 /opt/praxi-browser
  cp -a "${browser_work}/chrome-${platform}/." /opt/praxi-browser/
  chown -R root:root /opt/praxi-browser
  rm -rf "${browser_work}"
  # Ubuntu 24.04 restricts user namespaces to named AppArmor applications.
  # Same userns-only profile as Ubuntu's /etc/apparmor.d/chrome; keep the
  # browser sandbox enabled and leave all systemd protections in place.
  if [[ -f /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]] && command -v apparmor_parser >/dev/null 2>&1; then
    cat > /etc/apparmor.d/praxi-browser <<'PROFILE'
abi <abi/4.0>,
include <tunables/global>
profile praxi-browser /opt/praxi-browser/chrome flags=(unconfined) {
  userns,
}
PROFILE
    apparmor_parser -r /etc/apparmor.d/praxi-browser
  fi
}

# Printing is outside the dedicated Praxi host's role. Include socket units,
# otherwise a disabled cupsd can be activated again by its socket.
disable_print_services() {
  local unit
  for unit in cups.service cups.socket cups.path cups-browsed.service; do
    if systemctl cat "${unit}" >/dev/null 2>&1; then
      log "disabling unrelated print unit ${unit}"
      systemctl disable --now "${unit}"
    fi
  done
  if command -v snap >/dev/null 2>&1 && snap list cups >/dev/null 2>&1; then
    snap stop --disable cups
  fi
}

install_packages() {
  local pkgs_apt=(curl ca-certificates tar libdbus-1-3)
  # bot and bot-node link libssl.so.3 (native-tls via iroh/rmcp). curl usually
  # pulls it in; openssl-libs is the dnf name. The apt name is chosen below.
  local pkgs_dnf=(curl ca-certificates tar dbus-libs openssl-libs)
  if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
    pkgs_apt+=(xvfb xauth at-spi2-core x11-utils fonts-liberation fonts-noto-core)
    pkgs_dnf+=(xorg-x11-server-Xvfb xauth at-spi2-core liberation-fonts)
  fi
  if [[ "${WITH_DESKTOP_SESSION}" -eq 1 ]]; then
    pkgs_apt+=(dbus dbus-x11 xfwm4 xfce4-panel xfdesktop4 xfconf xfce4-settings
      xfce4-terminal thunar desktop-file-utils x11-xserver-utils xdg-utils)
    pkgs_dnf+=(dbus-daemon dbus-x11 xfwm4 xfce4-panel xfdesktop xfconf xfce4-settings
      xfce4-terminal thunar desktop-file-utils xdg-utils xdpyinfo xset)
  fi
  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -y
    if [[ "${WITH_DESKTOP_SESSION}" -eq 1 ]]; then
      local pkg missing_pkgs=()
      for pkg in "${pkgs_apt[@]}"; do
        if ! apt_candidate "${pkg}"; then
          missing_pkgs+=("${pkg}")
        fi
      done
      if [[ "${#missing_pkgs[@]}" -gt 0 ]]; then
        die "missing required apt package candidate(s) on ${ARCH}: ${missing_pkgs[*]}"
      fi
      for pkg in libatk-adaptor libgail-common libglib2.0-bin tango-icon-theme; do
        if apt_candidate "${pkg}"; then
          pkgs_apt+=("${pkg}")
        else
          warn "optional desktop package '${pkg}' has no candidate on ${ARCH}; continuing"
        fi
      done
    fi
    apt-get install -y --no-install-recommends "${pkgs_apt[@]}"
    # Debian 12 / Ubuntu: libssl3. Debian 13 renamed it to libssl3t64.
    if apt_candidate libssl3; then
      apt-get install -y --no-install-recommends libssl3
    elif apt_candidate libssl3t64; then
      apt-get install -y --no-install-recommends libssl3t64
    else
      warn "no libssl3 package found; the binaries need libssl.so.3"
    fi
    if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
      # Only Debian's native package is accepted. Ubuntu's virtual/transitional
      # chromium packages can resolve to snap even when apt-cache show exits 0.
      local distro_id
      # shellcheck source=/dev/null
      distro_id="$(. /etc/os-release; printf '%s' "${ID}")"
      if [[ "${distro_id}" == "debian" ]] && apt_candidate chromium; then
        apt-get install -y --no-install-recommends chromium
      else
        install_cft
      fi
    fi
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y --setopt=install_weak_deps=False "${pkgs_dnf[@]}"
    if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
      dnf install -y --setopt=install_weak_deps=False chromium || die "install a non-snap Chromium and re-run with --skip-packages"
    fi
  else
    warn "unknown package manager; install curl, tar, libdbus, libssl.so.3 (and Xvfb + Chromium for --with-desktop) yourself"
  fi
}

# ---------------------------------------------------------- verify release
# curl/gh must already be trusted and installed. Do not change packages, users,
# services or installation paths until the private copy passes verification.
command -v gh >/dev/null 2>&1 || die "trusted GitHub CLI (gh) is required; install it independently from https://cli.github.com"
# Disable optional CLI update checks as well as explicit downloads offline.
if [[ "${OFFLINE}" -eq 1 ]]; then
  export GH_NO_UPDATE_NOTIFIER=1 GH_NO_EXTENSION_UPDATE_NOTIFIER=1
fi
gh_version="$(gh --version)" || die "could not determine GitHub CLI version"
gh_version="${gh_version%%$'\n'*}"
[[ "$gh_version" =~ ^gh[[:space:]]version[[:space:]]([0-9]+)\.([0-9]+)\.([0-9]+)([[:space:]]|$) ]] \
  || die "GitHub CLI >= ${MIN_GH_VERSION} is required (unrecognized version)"
(( 10#${BASH_REMATCH[1]} > 2 || (10#${BASH_REMATCH[1]} == 2 && 10#${BASH_REMATCH[2]} >= 80) )) \
  || die "GitHub CLI >= ${MIN_GH_VERSION} is required"
if [[ "${OFFLINE}" -eq 1 ]]; then
  [[ -n "${LOCAL_FILE}" && "${VERSION}" != latest && -n "${TRUSTED_ROOT}" ]] \
    || die "--offline requires --file, an explicit --version, and --custom-trusted-root"
  [[ "${VERIFY_ONLY}" -eq 1 || "${SKIP_PACKAGES}" -eq 1 ]] \
    || die "offline installation requires --skip-packages (install runtime dependencies beforehand)"
  [[ "${WITH_DESKTOP}" -eq 0 ]] || die "--offline cannot download desktop/browser packages"
fi
[[ -z "${TRUSTED_ROOT}" || -s "${TRUSTED_ROOT}" ]] || die "trusted root file is missing or empty"

[[ -z "${LOCAL_FILE}" || "${VERSION}" != "latest" ]] || die "--file requires an explicit --version tag"
if [[ "${VERSION}" == "latest" ]]; then
  # Resolve the tag before downloading. Never trust an arbitrary redirect host
  # or a mutable 'latest' identity as the expected signer/source ref.
  latest_url="$(curl -fsS --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 60 \
    -o /dev/null --write-out '%{redirect_url}' "https://github.com/${REPO}/releases/latest")" \
    || die "could not resolve the latest release tag"
  latest_prefix="https://github.com/${REPO}/releases/tag/"
  [[ "${latest_url}" == "${latest_prefix}"* ]] || die "latest release did not resolve to the expected repository"
  VERSION="${latest_url#"${latest_prefix}"}"
fi
[[ "${VERSION}" =~ ^v[0-9][0-9A-Za-z._+-]*$ ]] || die "invalid release tag (expected v followed by a version)"
[[ -z "${EXPECTED_SHA}" || "${EXPECTED_SHA}" =~ ^[0-9a-f]{64}$ ]] || die "invalid --sha256"

WORK="$(umask 077; mktemp -d)"
trap 'rm -rf "${WORK:?}"' EXIT
BASE="https://github.com/${REPO}/releases/download/${VERSION}"

download_release_asset() {
  curl -fSL --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 --max-time 300 \
    -o "${WORK}/$1" "${BASE}/$1" || die "could not download release asset $1 for ${VERSION}"
}

if [[ -n "${LOCAL_FILE}" ]]; then
  [[ -f "${LOCAL_FILE}" ]] || die "file not found: ${LOCAL_FILE}"
  cp -- "${LOCAL_FILE}" "${WORK}/${ASSET}"
  if [[ -z "${EXPECTED_SHA}" && -f "${LOCAL_FILE}.sha256" ]]; then
    EXPECTED_SHA="$(sha256_hex "$(awk 'NR==1 { print; exit }' "${LOCAL_FILE}.sha256")")"
  fi
  [[ -n "${LOCAL_BUNDLE}" ]] || LOCAL_BUNDLE="$(dirname -- "${LOCAL_FILE}")/${BUNDLE_ASSET}"
else
  log "downloading ${BASE}/${ASSET}"
  download_release_asset "${ASSET}"
  if [[ -z "${EXPECTED_SHA}" ]]; then
    download_release_asset "${ASSET}.sha256"
    EXPECTED_SHA="$(sha256_hex "$(awk 'NR==1 { print; exit }' "${WORK}/${ASSET}.sha256")")"
  fi
fi

if [[ -n "${EXPECTED_SHA}" ]]; then
  [[ "${EXPECTED_SHA}" =~ ^[0-9a-f]{64}$ ]] || die "invalid SHA-256 checksum"
  ACTUAL_SHA="$(sha256_hex "$(sha256sum "${WORK}/${ASSET}")")"
  [[ "${ACTUAL_SHA}" == "${EXPECTED_SHA}" ]] || die "SHA-256 mismatch (expected ${EXPECTED_SHA}, got ${ACTUAL_SHA})"
  log "checksum OK"
fi

if [[ -n "${LOCAL_BUNDLE}" ]]; then
  [[ -f "${LOCAL_BUNDLE}" ]] || die "provenance bundle not found: ${LOCAL_BUNDLE}"
  cp -- "${LOCAL_BUNDLE}" "${WORK}/${BUNDLE_ASSET}"
else
  download_release_asset "${BUNDLE_ASSET}"
fi

# A custom root is an explicit administrator trust decision. Never auto-select
# one from release assets or from the archive directory: fetch it independently
# with `gh attestation trusted-root` on a trusted online machine.
# Expand optional arrays as ${a[@]+"${a[@]}"}: bash < 4.4 (including macOS
# /bin/bash 3.2) aborts on "${a[@]}" of an empty array under set -u.
trust_args=()
if [[ -n "${TRUSTED_ROOT}" ]]; then
  cp -- "${TRUSTED_ROOT}" "${WORK}/trusted_root.jsonl"
  trust_args=(--custom-trusted-root "${WORK}/trusted_root.jsonl")
fi
# Identity constraints remain fixed in both online and offline verification.
gh attestation verify "${WORK}/${ASSET}" --bundle "${WORK}/${BUNDLE_ASSET}" ${trust_args[@]+"${trust_args[@]}"} \
  --repo "${REPO}" --hostname github.com \
  --signer-workflow "${SIGNER_WORKFLOW}" \
  --source-ref "refs/tags/${VERSION}" --deny-self-hosted-runners \
  --cert-oidc-issuer https://token.actions.githubusercontent.com \
  --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256 \
  || die "provenance verification failed; refusing to install (a checksum is not proof of origin)"
log "verified release provenance for ${VERSION}"
if [[ "${VERIFY_ONLY}" -eq 1 ]]; then
  log "verification complete; no installation performed"
  exit 0
fi

# List members before extracting as root. Absolute paths and '..' components
# are rejected; the archive is extracted only into the temp dir.
reject_unsafe_tar_members "${WORK}/${ASSET}"
mkdir "${WORK}/x"
tar -xzf "${WORK}/${ASSET}" -C "${WORK}/x"
SRC="${WORK}/x/praxi-linux-${ARCH}"
[[ -x "${SRC}/bin/bot" && -x "${SRC}/bin/bot-node" ]] || die "tarball does not contain bin/bot and bin/bot-node"

# ------------------------------------------------------- privileged changes
if [[ "${SKIP_PACKAGES}" -eq 0 ]]; then
  log "installing OS packages"
  # Also retain private-download cleanup when package installation fails.
  trap 'disable_print_services || true; rm -rf "${WORK:?}"' EXIT
  install_packages
  trap 'rm -rf "${WORK:?}"' EXIT
fi
disable_print_services

BROWSER_BIN=""
for browser in /opt/praxi-browser/chrome /usr/bin/google-chrome /usr/bin/google-chrome-stable /usr/bin/chromium /usr/bin/chromium-browser; do
  [[ -x "${browser}" ]] || continue
  if [[ "$(readlink -f "${browser}")" == /snap/* ]] || head -c 8192 "${browser}" | grep -aqE 'snap run|/snap/'; then
    if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
      warn "ignoring unsupported snap browser: ${browser}"
    fi
    continue
  fi
  BROWSER_BIN="${browser}"
  break
done
if [[ "${WITH_DESKTOP}" -eq 1 ]]; then
  [[ -n "${BROWSER_BIN}" ]] || die "no non-snap browser found; snap-only installs are unsupported. Install Chrome/Chromium (see docs/guide/remote-host.md)"
fi

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
  local cmd="$1" bin="$2" envf="$3" property doctor_properties=""
  doctor_properties+=" --property=Slice=${SLICE_NAME}"
  while IFS= read -r property; do
    doctor_properties+=" --property=${property}"
  done <<< "${HARDENING}"
  if [[ -f "${UNIT_DIR}/praxi-xvfb.service" ]]; then
    doctor_properties+=" --property=JoinsNamespaceOf=praxi-xvfb.service --property=After=praxi-xvfb.service"
  fi
  if [[ -f "${UNIT_DIR}/praxi-desktop.service" ]]; then
    doctor_properties+=" --property=After=praxi-desktop.service"
  fi
  cat > "/usr/local/bin/${cmd}" <<WRAP
#!/bin/sh
# Runs ${PREFIX}/bin/${bin} as the ${SVC_USER} user with ${envf}.
inner='set -a; . ${envf}; set +a; exec "\$@"'
if [ "\$(id -u)" -eq 0 ]; then
  if [ "${cmd}" = praxi ] && [ "\${1:-}" = doctor ]; then
    exec systemd-run --quiet --wait --pipe --collect --unit="praxi-doctor-\$\$" \
      --property=User=${SVC_USER} --property=Group=${SVC_USER} \
      --property=WorkingDirectory=${DATA_DIR} --property=EnvironmentFile=${envf} \
      ${doctor_properties} ${PREFIX}/bin/${bin} "\$@"
  fi
  exec runuser -u ${SVC_USER} -- sh -c "\$inner" sh ${PREFIX}/bin/${bin} "\$@"
fi
exec sudo -u ${SVC_USER} sh -c "\$inner" sh ${PREFIX}/bin/${bin} "\$@"
WRAP
  chmod 0755 "/usr/local/bin/${cmd}"
}

set_env_kv() { # <file> <key> <value>
  local file="$1" key="$2" val="$3"
  sed -i "/^${key}=/d" "${file}"
  printf '%s=%s\n' "${key}" "${val}" >> "${file}"
}

remove_env_key() { # <file> <key>
  local file="$1" key="$2"
  sed -i "/^${key}=/d" "${file}"
}

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
      echo "BOT_MAX_COMPUTER_RUNS=1"
      echo "BOT_BROWSER_IDLE_SECONDS=120"
    } > "${ENV_FILE}"
  )
else
  log "keeping existing ${ENV_FILE}"
  if [[ "${PORT_SET}" -eq 1 ]]; then
    set_env_kv "${ENV_FILE}" BOT_PORT "${PORT}"
  elif grep -q '^BOT_PORT=' "${ENV_FILE}"; then
    PORT="$(grep '^BOT_PORT=' "${ENV_FILE}" | tail -n1 | cut -d= -f2- | tr -d '[:space:]')"
    [[ "${PORT}" =~ ^[0-9]+$ && "${PORT}" -ge 1 && "${PORT}" -le 65535 ]] || die "invalid BOT_PORT in ${ENV_FILE}"
  else
    echo "BOT_PORT=${PORT}" >> "${ENV_FILE}"
  fi
fi

DESKTOP_ACTIVE=0
DESKTOP_SESSION_ACTIVE=0
if [[ "${WITH_DESKTOP}" -eq 1 || "${KEEP_DESKTOP}" -eq 1 ]] && [[ "${WITHOUT_DESKTOP}" -eq 0 ]]; then
  DESKTOP_ACTIVE=1
fi
if [[ "${WITH_DESKTOP_SESSION}" -eq 1 || "${KEEP_DESKTOP_SESSION}" -eq 1 ]] && [[ "${WITHOUT_DESKTOP_SESSION}" -eq 0 ]]; then
  DESKTOP_SESSION_ACTIVE=1
  DESKTOP_ACTIVE=1
fi

if [[ "${DESKTOP_ACTIVE}" -eq 1 ]]; then
  if [[ "${DISPLAY_SET}" -eq 0 ]] && grep -q '^DISPLAY=:[0-9][0-9]*$' "${ENV_FILE}"; then
    DISPLAY_NUM="$(grep '^DISPLAY=' "${ENV_FILE}" | tail -n1 | cut -d= -f2-)"
  else
    set_env_kv "${ENV_FILE}" DISPLAY "${DISPLAY_NUM}"
  fi
  set_env_kv "${ENV_FILE}" XAUTHORITY "${XAUTH_FILE}"
else
  remove_env_key "${ENV_FILE}" DISPLAY
  remove_env_key "${ENV_FILE}" XAUTHORITY
fi

if [[ "${DESKTOP_SESSION_ACTIVE}" -eq 1 ]]; then
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

# Node-agent environment. Rewritten every run so it cannot carry
# BOT_SECRETS_PASSPHRASE (that stays in praxi.env for the daemon only).
log "writing ${NODE_ENV_FILE}"
(
  umask 077
  {
    echo "BOT_DATA_DIR=${DATA_DIR}"
    echo "HOME=${DATA_DIR}"
    if [[ "${DESKTOP_ACTIVE}" -eq 1 ]]; then
      echo "DISPLAY=${DISPLAY_NUM}"
      echo "XAUTHORITY=${XAUTH_FILE}"
    fi
    if [[ "${DESKTOP_SESSION_ACTIVE}" -eq 1 ]]; then
      echo "DBUS_SESSION_BUS_ADDRESS=unix:path=${DBUS_SOCKET}"
      echo "XDG_SESSION_TYPE=x11"
      echo "NO_AT_BRIDGE=0"
      echo "GTK_MODULES=gail:atk-bridge"
      echo "QT_ACCESSIBILITY=1"
      echo "QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1"
      echo "GNOME_ACCESSIBILITY=1"
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

cat > "${UNIT_DIR}/praxi.service" <<UNIT
[Unit]
Description=Praxi-Bot daemon (bot serve, 127.0.0.1 only)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Slice=${SLICE_NAME}
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
Slice=${SLICE_NAME}
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

write_desktop_libexec() {
  install -d -m 0755 "${LIBEXEC_DIR}"

  cat > "${LIBEXEC_DIR}/praxi-xauth-init.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
disp="${1:?display required}"
xauth_file="${2:?Xauthority path required}"
mkdir -p "$(dirname "${xauth_file}")"
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

  cat > "${LIBEXEC_DIR}/praxi-wait-x11.sh" <<'EOF'
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

  cat > "${LIBEXEC_DIR}/praxi-wait-desktop.sh" <<'EOF'
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

  cat > "${LIBEXEC_DIR}/praxi-xfce-session.sh" <<'EOF'
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
  cat > "${XDG_DATA_HOME}/applications/praxi-browser.desktop" <<DESKTOP
[Desktop Entry]
Version=1.0
Type=Application
Name=Web Browser
Comment=Launch browser manually (lazy, not autostarted)
Exec=${browser_bin} --no-first-run --no-default-browser-check --disable-dev-shm-usage --disable-gpu %U
Terminal=false
Categories=Network;WebBrowser;
StartupNotify=true
DESKTOP
fi
if [[ ! -f "${XDG_CONFIG_HOME}/xfce4/helpers.rc" ]]; then
  {
    echo "TerminalEmulator=xfce4-terminal"
    echo "FileManager=thunar"
    if [[ -n "${browser_bin}" ]]; then
      echo "WebBrowser=praxi-browser"
    fi
  } > "${XDG_CONFIG_HOME}/xfce4/helpers.rc"
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
trap cleanup EXIT INT TERM

dbus-daemon --session --address="unix:path=${DBUS_SOCKET}" --nofork --nopidfile &
DBUS_PID=$!

for _ in $(seq 1 50); do
  if [[ -S "${DBUS_SOCKET}" ]]; then
    break
  fi
  sleep 0.1
done
[[ -S "${DBUS_SOCKET}" ]] || { echo "error: session D-Bus socket did not appear" >&2; exit 1; }

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

wait -n "${DBUS_PID}" "${XFWM_PID}" "${XFDESKTOP_PID}" "${XFPANEL_PID}"
EOF

  chmod 0755 \
    "${LIBEXEC_DIR}/praxi-xauth-init.sh" \
    "${LIBEXEC_DIR}/praxi-wait-x11.sh" \
    "${LIBEXEC_DIR}/praxi-wait-desktop.sh" \
    "${LIBEXEC_DIR}/praxi-xfce-session.sh"
}

write_desktop_dropins() {
  local svc
  for svc in praxi praxi-node; do
    install -d "${UNIT_DIR}/${svc}.service.d"
    # Share the private /tmp (X11 socket) with the display service.
    if [[ "${DESKTOP_SESSION_ACTIVE}" -eq 1 ]]; then
      cat > "${UNIT_DIR}/${svc}.service.d/desktop.conf" <<UNIT
[Unit]
Requires=praxi-xvfb.service praxi-desktop.service
After=praxi-xvfb.service praxi-desktop.service
JoinsNamespaceOf=praxi-xvfb.service
UNIT
    else
      cat > "${UNIT_DIR}/${svc}.service.d/desktop.conf" <<UNIT
[Unit]
Requires=praxi-xvfb.service
After=praxi-xvfb.service
JoinsNamespaceOf=praxi-xvfb.service
UNIT
    fi
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
Restart=on-failure
RestartSec=3
TimeoutStopSec=15
${HARDENING}

[Install]
WantedBy=multi-user.target
UNIT
}

write_desktop_unit() {
  cat > "${UNIT_DIR}/praxi-desktop.service" <<UNIT
[Unit]
Description=Praxi minimal Xfce desktop session (${DISPLAY_NUM})
Requires=praxi-xvfb.service
After=praxi-xvfb.service
PartOf=praxi-xvfb.service
JoinsNamespaceOf=praxi-xvfb.service

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
${BROWSER_BIN:+Environment=BOT_BROWSER_PATH=${BROWSER_BIN}}
ExecStart=${LIBEXEC_DIR}/praxi-xfce-session.sh
ExecStartPost=${LIBEXEC_DIR}/praxi-wait-desktop.sh ${DBUS_SOCKET}
Restart=on-failure
RestartSec=3
TimeoutStopSec=15
KillMode=control-group
${HARDENING}

[Install]
WantedBy=multi-user.target
UNIT
}

if [[ "${WITHOUT_DESKTOP_SESSION}" -eq 1 && -f "${UNIT_DIR}/praxi-desktop.service" ]]; then
  log "removing praxi-desktop.service"
  systemctl disable --now praxi-desktop.service >/dev/null 2>&1 || true
  rm -f "${UNIT_DIR}/praxi-desktop.service"
fi
if [[ "${WITHOUT_DESKTOP}" -eq 1 && -f "${UNIT_DIR}/praxi-xvfb.service" ]]; then
  log "removing praxi-xvfb.service and DISPLAY (--without-desktop)"
  systemctl disable --now praxi-xvfb.service >/dev/null 2>&1 || true
  rm -f "${UNIT_DIR}/praxi-xvfb.service"
fi
if [[ "${DESKTOP_ACTIVE}" -eq 1 ]]; then
  if [[ "${KEEP_DESKTOP}" -eq 1 && -f "${UNIT_DIR}/praxi-xvfb.service" ]]; then
    xvfb_line="$(grep '^ExecStart=' "${UNIT_DIR}/praxi-xvfb.service" | tail -n1 || true)"
    if [[ "${SCREEN_SET}" -eq 0 && ! ( "${WITH_DESKTOP_SESSION}" -eq 1 && "${KEEP_DESKTOP_SESSION}" -eq 0 && ! -f "${UNIT_DIR}/praxi-desktop.service" ) ]]; then
      if [[ "${xvfb_line}" =~ -screen[[:space:]]+0[[:space:]]+([0-9]+x[0-9]+x[0-9]+) ]]; then
        SCREEN_SPEC="${BASH_REMATCH[1]}"
      fi
    fi
    # DISPLAY in the env file wins. If it was never recorded, keep the
    # display number already on the unit and copy it into both env files.
    if [[ "${DISPLAY_SET}" -eq 0 ]] && ! grep -q '^DISPLAY=:[0-9][0-9]*$' "${ENV_FILE}"; then
      if [[ "${xvfb_line}" =~ (^|[[:space:]])(:[0-9]+)([[:space:]]|$) ]]; then
        DISPLAY_NUM="${BASH_REMATCH[2]}"
        set_env_kv "${ENV_FILE}" DISPLAY "${DISPLAY_NUM}"
        set_env_kv "${NODE_ENV_FILE}" DISPLAY "${DISPLAY_NUM}"
      fi
    fi
  fi
  write_desktop_libexec
  write_xvfb_unit
  if [[ "${DESKTOP_SESSION_ACTIVE}" -eq 1 ]]; then
    write_desktop_unit
  fi
  write_desktop_dropins
fi

write_wrapper praxi bot "${ENV_FILE}"
write_wrapper praxi-node bot-node "${NODE_ENV_FILE}"

systemctl daemon-reload
systemctl enable praxi.service >/dev/null
if [[ "${DESKTOP_ACTIVE}" -eq 1 ]]; then
  systemctl enable praxi-xvfb.service >/dev/null
fi
if [[ "${DESKTOP_SESSION_ACTIVE}" -eq 1 ]]; then
  systemctl enable praxi-desktop.service >/dev/null
fi

START_FAILED=0
if [[ "${NO_START}" -eq 0 ]]; then
  if [[ "${DESKTOP_ACTIVE}" -eq 1 ]]; then
    systemctl restart praxi-xvfb.service || START_FAILED=1
  fi
  if [[ "${DESKTOP_SESSION_ACTIVE}" -eq 1 ]]; then
    systemctl restart praxi-desktop.service || START_FAILED=1
  fi
  systemctl restart praxi.service || START_FAILED=1
  ready=0
  consecutive=0
  for ((attempt=0; attempt<30; attempt++)); do
    if systemctl is-active --quiet praxi.service && \
      health="$(curl -fsS --noproxy '*' --connect-timeout 1 --max-time 2 "http://127.0.0.1:${PORT}/v1/health")" && \
      [[ "${health}" =~ \"status\"[[:space:]]*:[[:space:]]*\"(ok|setup_required)\" ]] && \
      [[ "${health}" == *'"version"'* ]]; then
      consecutive=$((consecutive + 1))
      if [[ "${consecutive}" -ge 2 ]]; then ready=1; break; fi
    else
      consecutive=0
    fi
    sleep 1
  done
  [[ "${ready}" -eq 1 ]] || START_FAILED=1
  if [[ "${DESKTOP_ACTIVE}" -eq 1 ]]; then
    systemctl is-active --quiet praxi-xvfb.service || START_FAILED=1
  fi
  if [[ "${DESKTOP_SESSION_ACTIVE}" -eq 1 ]]; then
    systemctl is-active --quiet praxi-desktop.service || START_FAILED=1
  fi
  if [[ "${NODE_WAS_ACTIVE}" -eq 1 ]]; then
    systemctl restart praxi-node.service || START_FAILED=1
    systemctl is-active --quiet praxi-node.service || START_FAILED=1
  fi
  if [[ "${START_FAILED}" -ne 0 ]]; then
    journalctl -u praxi -u praxi-node -u praxi-desktop -u praxi-xvfb -n 50 --no-pager >&2
    die "files installed, but service readiness failed (bounded wait); see journal above"
  fi
  log "daemon ready on loopback (health may report setup_required until provider setup)"
else
  log "files installed and units enabled; startup/readiness skipped (--no-start)"
fi

# ------------------------------------------------------------ next steps
cat <<EOF

Praxi-Bot is installed.

  Daemon:   praxi.service, configured for 127.0.0.1:${PORT} only (never expose this port)
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
  5. Update later by re-running this script. Existing praxi-xvfb/praxi-desktop
     units and DISPLAY stay in place; pass --without-desktop-session to remove
     only the Xfce session, or --without-desktop to remove both.
     Remove the install with: install-linux.sh --uninstall

See docs/guide/remote-host.md for firewall notes (Oracle Cloud) and security guidance.
EOF

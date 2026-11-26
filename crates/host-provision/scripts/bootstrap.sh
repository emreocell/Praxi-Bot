# shellcheck shell=bash
# Praxi one-command server setup. The desktop app shows this text; the person
# pastes it into the provider's web console (or any SSH session) on the server.
# It runs as root and does four things, each printed as it happens:
#   1. makes sure the GitHub CLI (2.80+) is there, because it checks signatures
#   2. downloads the Praxi installer for exactly the release the app was built from
#   3. VERIFIES the installer's signed build provenance before running it
#   4. installs the always-on preset and prints ONE pairing code (valid 30 minutes, one use)
# The only value filled in by the app is the release tag on the first line. Nothing private is in here.

# The GitHub CLI's package-signing key (see docs/install_linux.md in github.com/cli/cli; it also signs
# the repository metadata). Only this fingerprint is trusted: the downloaded key file may hold other
# keys, and none of them is installed. If GitHub rotates the key this stops with a message instead of
# trusting a new key blindly.
praxi_gh_key_fpr=7F38BBB59D064DBCB3D84D725612B36462313325

# praxi_pin_gh_key <downloaded key file> <output key file>
# Writes only the pinned key (with its subkeys) to the output file; fails when it is not in the download.
praxi_pin_gh_key() {
  local home found
  home="$(mktemp -d)" || return 1
  chmod 700 "${home}"
  if gpg --homedir "${home}" --batch --quiet --import "$1" >/dev/null 2>&1 \
    && found="$(gpg --homedir "${home}" --batch --with-colons --fingerprint 2>/dev/null \
      | awk -F: '$1 == "pub" {primary = 1; next} primary && $1 == "fpr" {print $10; primary = 0}')" \
    && printf '%s\n' "${found}" | grep -qx "${praxi_gh_key_fpr}" \
    && gpg --homedir "${home}" --batch --export "${praxi_gh_key_fpr}" >"$2" 2>/dev/null \
    && [ -s "$2" ]; then
    rm -rf "${home}"
    return 0
  fi
  rm -rf "${home}"
  rm -f "$2"
  return 1
}

# praxi_clean_env <home> <command> [args...]
# Runs a command with a clean environment: fixed PATH, an isolated HOME and GitHub CLI config, no
# tokens or other variables from the caller. Only proxy settings are kept. Extra NAME=value words
# before the command are passed on as they are (env syntax).
praxi_clean_env() {
  local home="$1" keep v
  shift
  keep=()
  for v in HTTPS_PROXY https_proxy NO_PROXY no_proxy; do
    [ -z "${!v:-}" ] || keep+=("${v}=${!v}")
  done
  /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME="${home}" GH_CONFIG_DIR="${home}/gh-config" \
    GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 ${keep[@]+"${keep[@]}"} "$@"
}

# praxi_run_installer <install-linux.sh> <release tag>
# Runs the verified installer as root through praxi_clean_env. The installer then looks for the
# GitHub CLI at /usr/bin/gh only, so nothing planted in the caller's PATH or home can answer for its
# signature check.
praxi_run_installer() {
  praxi_clean_env "$(dirname "$1")" DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l NEEDRESTART_SUSPEND=1 \
    bash "$1" --version "$2" --headless
}

praxi_main() {
  set -euo pipefail
  say() { printf '\n[praxi] %s\n' "$*"; }
  fail() { printf '\n[praxi] STOP: %s\n' "$*" >&2; exit 1; }
  [ "$(id -u)" -eq 0 ] || fail "this must run as root (use sudo)"
  case "$(uname -m)" in x86_64 | aarch64 | arm64) ;; *) fail "this computer type is not supported (need x86_64 or aarch64)" ;; esac
  [ -d /run/systemd/system ] || fail "this system does not use systemd, so it cannot run Praxi as a service"
  if ! command -v curl >/dev/null 2>&1; then
    say "preparing the download tool"
    if command -v apt-get >/dev/null 2>&1; then
      apt-get update -qq
      DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l NEEDRESTART_SUSPEND=1 apt-get install -y -qq curl ca-certificates
    elif command -v dnf >/dev/null 2>&1; then
      dnf install -y -q curl ca-certificates
    else
      fail "curl is missing; install curl and ca-certificates with your system's package manager, then run this again"
    fi
  fi
  curl -fsI --max-time 15 --proto '=https' --proto-redir '=https' --tlsv1.2 https://github.com >/dev/null 2>&1 \
    || fail "this server cannot reach github.com; check its internet access"

  need_gh=1
  if [ -x /usr/bin/gh ]; then
    have="$(/usr/bin/gh --version | awk 'NR==1 {print $3}')"
    [ "$(printf '%s\n%s\n' 2.80.0 "${have}" | sort -V | head -n1)" = 2.80.0 ] && need_gh=0
  fi
  if [ "${need_gh}" -eq 1 ]; then
    say "step 1 of 4: installing the GitHub CLI (used only to check signatures)"
    if command -v apt-get >/dev/null 2>&1; then
      export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l NEEDRESTART_SUSPEND=1
      apt-get update -qq
      apt-get install -y -qq curl ca-certificates gnupg
      keys="$(mktemp -d)"
      curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 --max-time 60 \
        https://cli.github.com/packages/githubcli-archive-keyring.gpg -o "${keys}/download.gpg" \
        || fail "could not download the GitHub CLI signing key"
      praxi_pin_gh_key "${keys}/download.gpg" "${keys}/pinned.gpg" \
        || fail "the GitHub CLI signing key is not the one Praxi expects (${praxi_gh_key_fpr}); install the GitHub CLI (2.80 or newer) from https://cli.github.com yourself, then run this again"
      install -d -m 755 /etc/apt/keyrings
      install -m 644 "${keys}/pinned.gpg" /etc/apt/keyrings/githubcli-archive-keyring.gpg
      rm -rf "${keys}"
      echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
        > /etc/apt/sources.list.d/github-cli.list
      apt-get update -qq
      apt-get install -y -qq gh
    elif command -v dnf >/dev/null 2>&1; then
      dnf install -y -q curl ca-certificates gnupg2
      keys="$(mktemp -d)"
      curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 --max-time 60 \
        https://cli.github.com/packages/githubcli-archive-keyring.asc -o "${keys}/download.asc" \
        || fail "could not download the GitHub CLI signing key"
      praxi_pin_gh_key "${keys}/download.asc" "${keys}/pinned.gpg" \
        || fail "the GitHub CLI signing key is not the one Praxi expects (${praxi_gh_key_fpr}); install the GitHub CLI (2.80 or newer) from https://cli.github.com yourself, then run this again"
      install -d -m 755 /etc/pki/rpm-gpg
      install -m 644 "${keys}/pinned.gpg" /etc/pki/rpm-gpg/RPM-GPG-KEY-github-cli
      rm -rf "${keys}"
      rpm --import /etc/pki/rpm-gpg/RPM-GPG-KEY-github-cli
      printf '%s\n' '[gh-cli]' 'name=packages for the GitHub CLI' 'baseurl=https://cli.github.com/packages/rpm' \
        'enabled=1' 'gpgcheck=1' 'gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-github-cli' >/etc/yum.repos.d/gh-cli.repo
      dnf install -y -q gh --repo gh-cli
    else
      fail "no apt-get or dnf found; install the GitHub CLI (2.80 or newer) yourself, then run this again"
    fi
  else
    say "step 1 of 4: the GitHub CLI is already there"
  fi

  work="$(mktemp -d)"
  trap 'rm -rf "${work}"' EXIT
  base="https://github.com/emreocell/Praxi-Bot/releases/download/${PRAXI_TAG}"
  say "step 2 of 4: downloading Praxi ${PRAXI_TAG}"
  for asset in install-linux.sh praxi-linux.sigstore.json; do
    curl -fSL --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 15 --max-time 300 \
      "${base}/${asset}" -o "${work}/${asset}" \
      || fail "could not download ${asset} for ${PRAXI_TAG}; is that release published?"
  done

  say "step 3 of 4: checking the signature (nothing runs if this fails)"
  praxi_clean_env "${work}" \
    /usr/bin/gh attestation verify "${work}/install-linux.sh" --bundle "${work}/praxi-linux.sigstore.json" \
    --repo emreocell/Praxi-Bot --hostname github.com \
    --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
    --source-ref "refs/tags/${PRAXI_TAG}" --deny-self-hosted-runners \
    --cert-oidc-issuer https://token.actions.githubusercontent.com \
    --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256 \
    || fail "the signature check failed, so nothing was installed"

  say "step 4 of 4: installing Praxi (a few minutes)"
  praxi_run_installer "${work}/install-linux.sh" "${PRAXI_TAG}"

  port=47821
  if [ -r /etc/praxi/praxi.env ] && grep -q '^BOT_PORT=' /etc/praxi/praxi.env; then
    port="$(grep '^BOT_PORT=' /etc/praxi/praxi.env | tail -n1 | cut -d= -f2- | tr -cd '0-9')"
  fi
  ticket=""
  for _ in $(seq 1 40); do
    ticket="$(runuser -u praxi -- env BOT_DATA_DIR=/var/lib/praxi BOT_PORT="${port}" HOME=/var/lib/praxi \
      /opt/praxi/bin/bot host invite --role desktop --ttl-secs 1800 2>/dev/null)" && break
    ticket=""
    sleep 3
  done
  [ -n "${ticket}" ] || fail "Praxi is installed but pairing is not ready; run: sudo systemctl status praxi; then run: sudo praxi host invite --role desktop --ttl-secs 1800"
  printf '\n[praxi] DONE. Open Add server in Praxi and paste the line starting with bot-pair: (valid 30 minutes, works once):\n\n%s\n\n' "${ticket}"
}

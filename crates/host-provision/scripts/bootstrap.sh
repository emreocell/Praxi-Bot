# shellcheck shell=bash
# Praxi one-command server setup. The desktop app shows this text; the person
# pastes it into the provider's web console (or any SSH session) on the server.
# It runs as root and does four things, each printed as it happens:
#   1. makes sure the GitHub CLI (2.80+) is there, because it checks signatures
#   2. downloads the Praxi installer for exactly the release the app was built from
#   3. VERIFIES the installer's signed build provenance before running it
#   4. installs the always-on preset and prints ONE pairing code (valid 30 minutes, one use)
# The only value filled in by the app is the release tag on the first line. Nothing private is in here.
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
  curl -fsI --max-time 15 --proto '=https' --tlsv1.2 https://github.com >/dev/null 2>&1 \
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
      apt-get install -y -qq curl ca-certificates
      install -d -m 755 /etc/apt/keyrings
      curl -fsSL --proto '=https' --proto-redir '=https' --tlsv1.2 https://cli.github.com/packages/githubcli-archive-keyring.gpg \
        -o /etc/apt/keyrings/githubcli-archive-keyring.gpg
      chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg
      echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
        > /etc/apt/sources.list.d/github-cli.list
      apt-get update -qq
      apt-get install -y -qq gh
    elif command -v dnf >/dev/null 2>&1; then
      dnf install -y -q curl 'dnf-command(config-manager)' || true
      dnf config-manager --add-repo https://cli.github.com/packages/rpm/gh-cli.repo >/dev/null 2>&1 \
        || dnf config-manager addrepo --from-repofile=https://cli.github.com/packages/rpm/gh-cli.repo
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
  /usr/bin/env -i PATH=/usr/bin:/bin HOME="${work}" GH_CONFIG_DIR="${work}" GH_PROMPT_DISABLED=1 \
    /usr/bin/gh attestation verify "${work}/install-linux.sh" --bundle "${work}/praxi-linux.sigstore.json" \
    --repo emreocell/Praxi-Bot --hostname github.com \
    --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
    --source-ref "refs/tags/${PRAXI_TAG}" --deny-self-hosted-runners \
    --cert-oidc-issuer https://token.actions.githubusercontent.com \
    --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256 \
    || fail "the signature check failed, so nothing was installed"

  say "step 4 of 4: installing Praxi (a few minutes)"
  DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l NEEDRESTART_SUSPEND=1 \
    bash "${work}/install-linux.sh" --version "${PRAXI_TAG}" --headless

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

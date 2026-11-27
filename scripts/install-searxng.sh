#!/usr/bin/env bash
# Install a private SearXNG search server next to Praxi on a Linux server, in one command:
#
#   sudo bash scripts/install-searxng.sh
#
# Why: the free search engines (DuckDuckGo, Bing) usually refuse servers and cloud machines as
# robots, so `web_search` finds nothing there. SearXNG is a search server you run yourself; it asks
# many engines for you and answers Praxi in JSON. It listens on 127.0.0.1 only: nothing outside
# the server can reach it.
#
# What it does (nothing else):
#   1. needs Docker (it does not install Docker for you; see docs/guide/web.md)
#   2. writes <dir>/settings.yml (JSON answers on, the rate limiter off, a random secret key)
#   3. starts the container `praxi-searxng` on 127.0.0.1:<port>, restarted automatically
#   4. asks it one real search and prints the address to give to Praxi
#
# Run it again at any time: it replaces the container and keeps your settings.yml and its key.
#
# Options (or the environment variables in brackets):
#   --dir <path>     where settings live            [SEARXNG_DIR,   default /opt/praxi/searxng]
#   --port <number>  local port                     [SEARXNG_PORT,  default 8080]
#   --image <ref>    container image to run         [SEARXNG_IMAGE, default docker.io/searxng/searxng:latest]
#   --uninstall      stop and remove the container (keeps <dir>)
set -euo pipefail

DIR="${SEARXNG_DIR:-/opt/praxi/searxng}"
PORT="${SEARXNG_PORT:-8080}"
IMAGE="${SEARXNG_IMAGE:-docker.io/searxng/searxng:latest}"
NAME="praxi-searxng"
UNINSTALL=0

die() {
  echo "error: $*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR="${2:?--dir needs a path}"; shift 2 ;;
    --port) PORT="${2:?--port needs a number}"; shift 2 ;;
    --image) IMAGE="${2:?--image needs an image}"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h | --help) sed -n '2,24p' "$0"; exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
done

case "$PORT" in
  '' | *[!0-9]*) die "--port must be a number" ;;
esac
if [ "$PORT" -lt 1024 ] || [ "$PORT" -gt 65535 ]; then
  die "--port must be between 1024 and 65535"
fi

command -v docker >/dev/null 2>&1 || die "Docker is not installed. Install it first (https://docs.docker.com/engine/install/), then run this again."
docker info >/dev/null 2>&1 || die "cannot talk to Docker: run this as root (sudo bash $0) or add your user to the docker group."

if [ "$UNINSTALL" -eq 1 ]; then
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  echo "Removed the container $NAME. Settings are kept in $DIR."
  exit 0
fi

# A port that something else already uses would make the container fail to start with a
# confusing message: say it plainly (our own container is replaced below, so it does not count).
if ! docker ps --format '{{.Names}}' | grep -qx "$NAME"; then
  if command -v ss >/dev/null 2>&1 && ss -ltn "sport = :$PORT" 2>/dev/null | grep -q LISTEN; then
    die "port $PORT is already in use; pick another with --port"
  fi
fi

mkdir -p "$DIR"
chmod 700 "$DIR"
SETTINGS="$DIR/settings.yml"

if [ -f "$SETTINGS" ]; then
  echo "Keeping the existing $SETTINGS"
else
  if command -v openssl >/dev/null 2>&1; then
    KEY="$(openssl rand -hex 32)"
  else
    KEY="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  fi
  [ "${#KEY}" -ge 32 ] || die "could not create a random secret key"
  umask 077
  cat >"$SETTINGS" <<EOF
# Written by Praxi's scripts/install-searxng.sh. Safe to edit; the installer never overwrites it.
use_default_settings: true
general:
  instance_name: "Praxi search"
server:
  secret_key: "$KEY"
  # Praxi is the only client and reaches this server on 127.0.0.1: no bot limiter, no image proxy.
  limiter: false
  image_proxy: false
search:
  # Praxi asks for JSON answers.
  formats:
    - html
    - json
EOF
  echo "Wrote $SETTINGS"
fi

docker pull "$IMAGE" >/dev/null || die "could not download $IMAGE (is this server online?)"
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d \
  --name "$NAME" \
  --restart unless-stopped \
  -p "127.0.0.1:$PORT:8080" \
  -v "$DIR:/etc/searxng" \
  -e "SEARXNG_BASE_URL=http://127.0.0.1:$PORT/" \
  "$IMAGE" >/dev/null || die "could not start the container (see: docker logs $NAME)"

URL="http://127.0.0.1:$PORT"
echo "Waiting for SearXNG to answer a real search..."
ok=0
for _ in $(seq 1 45); do
  if command -v curl >/dev/null 2>&1 \
    && curl -fsS --max-time 10 "$URL/search?q=praxi&format=json" 2>/dev/null | grep -q '"results"'; then
    ok=1
    break
  fi
  sleep 2
done
if [ "$ok" -ne 1 ]; then
  echo "SearXNG started but did not answer a JSON search in 90 seconds." >&2
  echo "Look at: docker logs $NAME" >&2
  echo "(an answer of HTTP 403 means 'json' is missing from search.formats in $SETTINGS)" >&2
  exit 1
fi

cat <<EOF

SearXNG is running and answers searches at $URL (this server only).

Tell Praxi about it, either way:
  - In the app: Settings > Web search > SearXNG, paste  $URL  and press "Save and test".
  - Or in <data_dir>/web.toml on this server:
        [search]
        searxng_url = "$URL"
    then restart Praxi.

Remove it later with:  sudo bash $0 --uninstall
EOF

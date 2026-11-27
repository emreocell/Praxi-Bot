#!/usr/bin/env bash
# Hermetic test of scripts/install-searxng.sh: a fake `docker` and `curl` on PATH, a temp settings
# directory, no root, no network, no real container. It checks which image the script is willing to
# download and start: only the official one, pinned by digest, or a moving tag the owner accepted.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SCRIPT="$ROOT/scripts/install-searxng.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# rep <char> <count>: the character repeated.
rep() { printf '%*s' "$2" '' | tr ' ' "$1"; }

mkdir -p "$WORK/bin"
export FAKE_LOG="$WORK/docker.log"
# The digest the fake Docker reports for whatever it pulled.
FAKE_DIGEST="$(rep b 64)"
export FAKE_DIGEST

cat >"$WORK/bin/docker" <<'EOF'
#!/usr/bin/env bash
echo "docker $*" >>"$FAKE_LOG"
case "$1" in
  image)
    # `docker image inspect --format ... <ref>`: the digest Docker holds for the image.
    echo "searxng/searxng@sha256:$FAKE_DIGEST"
    ;;
  run) echo fake-container-id ;;
esac
exit 0
EOF
cat >"$WORK/bin/curl" <<'EOF'
#!/usr/bin/env bash
echo '{"query":"praxi","results":[]}'
EOF
chmod +x "$WORK/bin/docker" "$WORK/bin/curl"

fail=0
check() {
  # check <name> <condition result: 0 = good>
  if [ "$2" -eq 0 ]; then
    echo "ok   - $1"
  else
    echo "FAIL - $1"
    fail=1
  fi
}

# run_installer <args...>: runs the script with the fakes first on PATH; sets OUT and CODE.
run_installer() {
  : >"$FAKE_LOG"
  set +e
  OUT="$(PATH="$WORK/bin:$PATH" bash "$SCRIPT" --dir "$WORK/settings" --port 18999 "$@" 2>&1)"
  CODE=$?
  set -e
}

docker_calls() { grep -c "^docker $1" "$FAKE_LOG" || true; }
refused_before_download() {
  [ "$CODE" -ne 0 ] && [ "$(docker_calls pull)" -eq 0 ] && [ "$(docker_calls run)" -eq 0 ]
}

PIN_A="sha256:$(rep a 64)"
OFFICIAL="docker.io/searxng/searxng"

# --- nothing says which image: refuse, and say how -------------------------------------------
run_installer
check "no --image and no --allow-latest is refused before any download" "$(refused_before_download && echo 0 || echo 1)"
check "the refusal names both ways out" "$(grep -q -- '--allow-latest' <<<"$OUT" && grep -q -- '@sha256:' <<<"$OUT" && echo 0 || echo 1)"

run_installer --image "$OFFICIAL:latest"
check "an official tag without --allow-latest is refused" "$(refused_before_download && echo 0 || echo 1)"

# --- other images are never run, whatever the flags ------------------------------------------
for bad in \
  "evil/searxng@$PIN_A" \
  "docker.io/evil/searxng@$PIN_A" \
  "ghcr.io/searxng/searxng@$PIN_A" \
  "docker.io/searxng/searxng-evil@$PIN_A" \
  "evil.example/docker.io/searxng/searxng@$PIN_A" \
  "docker.io/searxng/searxng/extra@$PIN_A" \
  "localhost:5000/searxng/searxng@$PIN_A" \
  "alpine" \
  "alpine:latest" \
  "docker.io/searxng/searxng:latest --privileged"; do
  run_installer --allow-latest --image "$bad"
  check "image '$bad' is refused" "$(refused_before_download && echo 0 || echo 1)"
done

# --- a malformed digest or tag is refused -----------------------------------------------------
for bad in \
  "$OFFICIAL@sha256:abc" \
  "$OFFICIAL@sha256:$(rep A 64)" \
  "$OFFICIAL@sha512:$(rep a 64)" \
  "$OFFICIAL@$(rep a 64)" \
  "$OFFICIAL@sha256:$(rep a 63)g"; do
  run_installer --image "$bad"
  check "digest in '$bad' is refused" "$(refused_before_download && echo 0 || echo 1)"
done
run_installer --allow-latest --image "$OFFICIAL:late st;x"
check "an odd tag is refused" "$(refused_before_download && echo 0 || echo 1)"

# --- a pinned digest: pulled and run exactly as given -----------------------------------------
run_installer --image "$OFFICIAL@$PIN_A"
check "a pinned official image is refused when Docker holds another digest" "$([ "$CODE" -ne 0 ] && [ "$(docker_calls run)" -eq 0 ] && echo 0 || echo 1)"
FAKE_DIGEST="${PIN_A#sha256:}" run_installer --image "$OFFICIAL@$PIN_A"
check "a pinned official image starts" "$([ "$CODE" -eq 0 ] && echo 0 || echo 1)"
check "the pull names the digest" "$(grep -q "^docker pull $OFFICIAL@$PIN_A\$" "$FAKE_LOG" && echo 0 || echo 1)"
check "the container runs the digest" "$(grep "^docker run" "$FAKE_LOG" | grep -q -- " $OFFICIAL@$PIN_A\$" && echo 0 || echo 1)"
check "the digest is printed" "$(grep -q "$PIN_A" <<<"$OUT" && echo 0 || echo 1)"
FAKE_DIGEST="${PIN_A#sha256:}" run_installer --image "searxng/searxng@$PIN_A"
check "the short Docker Hub spelling is accepted and normalised" "$(grep -q "^docker pull $OFFICIAL@$PIN_A\$" "$FAKE_LOG" && [ "$CODE" -eq 0 ] && echo 0 || echo 1)"
FAKE_DIGEST="${PIN_A#sha256:}" run_installer --image "$OFFICIAL:2025.1.1@$PIN_A"
check "tag plus digest runs the digest" "$([ "$CODE" -eq 0 ] && grep "^docker run" "$FAKE_LOG" | grep -q -- " $OFFICIAL@$PIN_A\$" && echo 0 || echo 1)"
FAKE_DIGEST="${PIN_A#sha256:}" SEARXNG_IMAGE="$OFFICIAL@$PIN_A" run_installer
check "the environment variable is read the same way" "$([ "$CODE" -eq 0 ] && echo 0 || echo 1)"
SEARXNG_IMAGE="evil/image@$PIN_A" run_installer
check "and refused the same way" "$(refused_before_download && echo 0 || echo 1)"

# --- a moving tag, accepted on purpose: resolved and run by digest ----------------------------
run_installer --allow-latest
check "--allow-latest starts" "$([ "$CODE" -eq 0 ] && echo 0 || echo 1)"
check "it pulls the official latest tag" "$(grep -q "^docker pull $OFFICIAL:latest\$" "$FAKE_LOG" && echo 0 || echo 1)"
check "it runs the digest that was pulled, not the tag" "$(grep "^docker run" "$FAKE_LOG" | grep -q -- " $OFFICIAL@sha256:$FAKE_DIGEST\$" && ! grep "^docker run" "$FAKE_LOG" | grep -q ':latest' && echo 0 || echo 1)"
check "it tells how to pin that image next time" "$(grep -q -- "--image $OFFICIAL@sha256:$FAKE_DIGEST" <<<"$OUT" && echo 0 || echo 1)"
run_installer --allow-latest --image "$OFFICIAL:2025.1.1"
check "--allow-latest with a named official tag pulls that tag" "$(grep -q "^docker pull $OFFICIAL:2025.1.1\$" "$FAKE_LOG" && [ "$CODE" -eq 0 ] && echo 0 || echo 1)"
SEARXNG_ALLOW_LATEST=1 run_installer
check "SEARXNG_ALLOW_LATEST=1 is the same acknowledgement" "$([ "$CODE" -eq 0 ] && echo 0 || echo 1)"
SEARXNG_ALLOW_LATEST=yes run_installer
check "any other value is not" "$(refused_before_download && echo 0 || echo 1)"

# --- uninstall needs no image -----------------------------------------------------------------
run_installer --uninstall
check "--uninstall works without an image" "$([ "$CODE" -eq 0 ] && grep -q '^docker rm -f praxi-searxng' "$FAKE_LOG" && echo 0 || echo 1)"

exit "$fail"

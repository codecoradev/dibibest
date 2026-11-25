#!/usr/bin/env bash
# The local deploy rehearsal: builds the image from the root Dockerfile, starts the production shape on this machine
# (Traefik, the dibibest container with a named volume and a backup directory, a fake identity provider), runs the
# end-to-end checks in e2e.sh and tears everything down.
#
#   deploy/local/run.sh            full run (takes a few minutes: the idle-connection check waits)
#   deploy/local/run.sh --fast     skip the waits
#   deploy/local/run.sh --keep     leave the stack running afterwards (http://localhost:8088)
#
# Needs Docker with compose, curl and python3. Uses ports 8088 and 9000 on 127.0.0.1.
set -eu
cd "$(dirname "$0")"

KEEP=0
FAST=""
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    --fast) FAST="--fast" ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

COMPOSE="docker compose -f compose.yml"

# The backup directory is written by the server (uid 10001), which closes it to other users (mode 700) and owns what is
# in it, so the host user cannot always change or empty it: on Linux that would stop a second run. Root in a throw-away
# container can, and works the same on Docker Desktop and OrbStack, which map the owner on their own.
backups() {
  docker run --rm -v "$PWD/backups":/b alpine:3.22 sh -c "$1"
}
mkdir -p backups
backups 'chmod 777 /b; rm -f /b/dibibest-*.db /b/dibibest-*.partial'

$COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true

# tear down whatever was started, also when something below fails or the run is interrupted (unless --keep); the exit
# status of the run is the one the script had, whatever the cleanup does
cleanup() {
  rc=$?
  if [ "$KEEP" = "0" ]; then
    $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true
    backups 'rm -f /b/dibibest-*.db /b/dibibest-*.partial' >/dev/null 2>&1 || true
  fi
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

$COMPOSE up -d --build

printf 'waiting for the stack'
for _ in $(seq 1 90); do
  if curl -sf http://localhost:8088/readyz >/dev/null 2>&1; then break; fi
  printf '.'
  sleep 1
done
echo
curl -sf http://localhost:8088/readyz >/dev/null || { echo "the stack did not become ready"; $COMPOSE logs --tail 40; exit 1; }
# the fake identity provider starts a moment after the server: the sign-in checks need it at once
for _ in $(seq 1 30); do
  if curl -sf http://localhost:9000/api/auth/jwks >/dev/null 2>&1; then break; fi
  sleep 1
done
curl -sf http://localhost:9000/api/auth/jwks >/dev/null || { echo "the fake identity provider did not start"; $COMPOSE logs --tail 40 idp; exit 1; }

status=0
./e2e.sh $FAST || status=$?

if [ "$KEEP" != "0" ]; then
  echo "left running: http://localhost:8088 (stop it with: docker compose -f deploy/local/compose.yml down -v)"
fi
exit $status

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

mkdir -p backups
# On Linux the server (uid 10001) must be able to write to the host directory. It closes the directory to others
# itself once it can (mode 700); Docker Desktop and OrbStack map the owner on their own.
if [ "$(uname)" = "Linux" ]; then chmod 777 backups; fi

$COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true
rm -f backups/dibibest-*.db backups/dibibest-*.partial

$COMPOSE up -d --build

printf 'waiting for the stack'
for _ in $(seq 1 90); do
  if curl -sf http://localhost:8088/readyz >/dev/null 2>&1; then break; fi
  printf '.'
  sleep 1
done
echo
curl -sf http://localhost:8088/readyz >/dev/null || { echo "the stack did not become ready"; $COMPOSE logs --tail 40; exit 1; }

status=0
./e2e.sh $FAST || status=$?

if [ "$KEEP" = "0" ]; then
  $COMPOSE down -v --remove-orphans >/dev/null 2>&1
  rm -f backups/dibibest-*.db backups/dibibest-*.partial
else
  echo "left running: http://localhost:8088 (stop it with: docker compose -f deploy/local/compose.yml down -v)"
fi
exit $status

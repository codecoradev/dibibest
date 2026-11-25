#!/usr/bin/env bash
# End-to-end checks of the local rehearsal, through Traefik like a client on the internet would (deploy/local/run.sh
# starts the stack and calls this). Prints one line per check and exits non-zero if any failed.
#   e2e.sh --fast    skip the idle-connection check (it waits 3 x 12 s)
set -u
cd "$(dirname "$0")"

BASE=${BASE:-http://localhost:8088}
IDP=${IDP:-http://localhost:9000}
ISSUER="$IDP/api/auth"
COMPOSE="docker compose -f compose.yml"
CONTAINER=dibibest-local
FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

# only what Traefik logs from here on counts: while the stack is starting, Traefik can answer 502 before the backend is up
E2E_START=$(date -u +%Y-%m-%dT%H:%M:%SZ)

PASS=0
FAIL=0
FAILED=()
# leftovers of an interrupted run: the throw-away container of section 7 would keep the volume from being removed
cleanup() { [ -n "${CID:-}" ] && docker rm -f "$CID" >/dev/null 2>&1; rm -f "${JAR:-}" "${JAR2:-}" "${JAR4:-}" "${JAR5:-}"; }
trap cleanup EXIT
trap 'exit 130' INT TERM
pass() { PASS=$((PASS + 1)); printf '  ok    %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); FAILED+=("$1"); printf '  FAIL  %s\n' "$1"; [ -n "${2:-}" ] && printf '        %s\n' "$2"; return 0; }
expect_eq() { if [ "$2" = "$3" ]; then pass "$1"; else fail "$1" "got '$2', wanted '$3'"; fi; }
expect_has() { case "$2" in *"$3"*) pass "$1" ;; *) fail "$1" "missing '$3' in: ${2:0:300}" ;; esac; }
expect_not_has() { case "$2" in *"$3"*) fail "$1" "found '$3'" ;; *) pass "$1" ;; esac; }
status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
# a python expression over the parsed JSON on stdin, called `d`
jq_() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)" 2>/dev/null; }
# the Location header of a response (headers dumped by curl -D -)
location() { tr -d '\r' | awk 'tolower($1)=="location:" {print $2}'; }
# Run a shell snippet inside a throw-away container that has the backup directory at /b (read-only). The directory and
# the files in it belong to uid 10001 and are closed to other users, so on Linux the host user cannot read them
# directly; root in a container can.
in_backups() { docker run --rm -v "$PWD/backups":/b:ro alpine:3.22 sh -c "$1"; }

echo "1. the site through the proxy"
expect_eq "GET /healthz" "$(status "$BASE/healthz")" 200
expect_has "GET /readyz reports the WAL" "$(curl -s "$BASE/readyz")" '"wal":true'
expect_eq "GET / serves the web app" "$(status "$BASE/")" 200
expect_has "...as HTML" "$(curl -s -D - -o /dev/null "$BASE/" | tr -d '\r' | grep -i '^content-type')" "text/html"
code=$(status --path-as-is "$BASE/..%2f..%2fetc/passwd")
case "$code" in 400|404) pass "a path traversal attempt is refused ($code)" ;; *) fail "a path traversal attempt is refused" "got $code" ;; esac
expect_eq "an unknown API path is a JSON 404" "$(status "$BASE/v1/anything")" 404

echo "2. sign-in against the fake identity provider"
JAR=$(mktemp)
expect_eq "no user before signing in" "$(curl -s -b "$JAR" -c "$JAR" "$BASE/auth/me" | jq_ "d['user']")" "None"
LOC=$(curl -s -o /dev/null -D - -b "$JAR" -c "$JAR" "$BASE/auth/login?return_to=/app" | location)
expect_has "login redirects to the provider" "$LOC" "$IDP/api/auth/oauth2/authorize?"
expect_has "...with PKCE S256" "$LOC" "code_challenge_method=S256"
expect_has "...and the public address as the redirect" "$LOC" "redirect_uri=http%3A%2F%2Flocalhost%3A8088%2Fauth%2Fcallback"
CB=$(curl -s -o /dev/null -D - "$LOC" | location)
expect_has "the provider sends the browser back to the public address" "$CB" "$BASE/auth/callback?code="
HDR=$(curl -s -o /dev/null -D - -b "$JAR" -c "$JAR" "$CB" | tr -d '\r')
expect_has "the callback lands on the app" "$(echo "$HDR" | location)" "/app"
expect_has "...and sets a session cookie" "$HDR" "dibibest_session="
ME=$(curl -s -b "$JAR" -c "$JAR" "$BASE/auth/me")
expect_eq "the session names the user (email in lower case)" "$(echo "$ME" | jq_ "d['user']['email']")" "rehearsal@example.test"
# a callback with a state that was not issued to this browser is refused and signs nobody in
JAR2=$(mktemp)
LOC2=$(curl -s -o /dev/null -D - -b "$JAR2" -c "$JAR2" "$BASE/auth/login" | location)
CB2=$(curl -s -o /dev/null -D - "$LOC2" | location | sed 's/state=[^&]*/state=forged/')
code=$(status -b "$JAR2" -c "$JAR2" "$CB2")
case "$code" in 400|403) pass "a forged state is refused ($code)" ;; *) fail "a forged state is refused" "got $code" ;; esac
expect_eq "...and nobody is signed in" "$(curl -s -b "$JAR2" "$BASE/auth/me" | jq_ "d['user']")" "None"
# A code the provider refuses signs nobody in. The jar is copied before the callback, so the replay carries the same
# handshake cookie and the same state: the server accepts the state and goes on to the exchange, where the provider
# refuses the used code. (The fake provider makes a code single use, as a real one does.)
JAR4=$(mktemp); JAR5=$(mktemp)
LOC4=$(curl -s -o /dev/null -D - -b "$JAR4" -c "$JAR4" "$BASE/auth/login" | location)
CB4=$(curl -s -o /dev/null -D - "$LOC4" | location)
cp "$JAR4" "$JAR5"
FIRSTCB=$(curl -s -o /dev/null -D - -b "$JAR4" -c "$JAR4" "$CB4" | tr -d '\r')
expect_has "the first callback of a code signs in" "$FIRSTCB" "dibibest_session="
REPLAY=$(curl -s -o /dev/null -D - -b "$JAR5" -c "$JAR5" "$CB4" | tr -d '\r')
expect_eq "the provider's refusal of a used code is the server's own 502 (as in the TypeScript version)" "$(echo "$REPLAY" | head -1 | awk '{print $2}')" 502
expect_not_has "the same callback replayed does not set a session" "$REPLAY" "dibibest_session=eyJ"
expect_eq "...and the replaying browser is not signed in" "$(curl -s -b "$JAR5" "$BASE/auth/me" | jq_ "d['user']")" "None"
rm -f "$JAR4" "$JAR5"
code=$(status -X POST -H "Origin: http://evil.test" -b "$JAR" -c "$JAR" "$BASE/auth/logout")
expect_eq "a logout from another origin is refused" "$code" 403
expect_eq "...and the session stays" "$(curl -s -b "$JAR" "$BASE/auth/me" | jq_ "d['user']['email']")" "rehearsal@example.test"
code=$(status -X POST -H "Origin: $BASE" -b "$JAR" -c "$JAR" "$BASE/auth/logout")
expect_eq "logout from the site redirects (303)" "$code" 303
expect_eq "...ends the session" "$(curl -s -b "$JAR" -c "$JAR" "$BASE/auth/me" | jq_ "d['user']")" "None"
rm -f "$JAR" "$JAR2"

echo "3. the MCP endpoint with real tokens"
AUD="$BASE/mcp"
token() { curl -s "$IDP/dev/token?$1"; }
TOKEN=$(token "aud=$AUD")
post() { curl -s -X POST "$AUD" -H 'content-type: application/json' -H "authorization: Bearer $TOKEN" "$@"; }
postcode() { curl -s -o /dev/null -w '%{http_code}' -X POST "$AUD" -H 'content-type: application/json' "$@"; }
CHAL=$(curl -s -D - -o /dev/null -X POST "$AUD" -H 'content-type: application/json' -d '{}' | tr -d '\r' | grep -i '^www-authenticate')
expect_has "no token: 401 with the resource metadata address" "$CHAL" "resource_metadata=\"$BASE/.well-known/oauth-protected-resource/mcp\""
expect_eq "a token for another resource is refused" "$(postcode -H "authorization: Bearer $(token "aud=https://evil.example/mcp")" -d '{}')" 401
expect_eq "a token from another issuer is refused" "$(postcode -H "authorization: Bearer $(token "aud=$AUD&iss=http://evil.example")" -d '{}')" 401
expect_eq "an expired token is refused" "$(postcode -H "authorization: Bearer $(token "aud=$AUD&exp_in=-600")" -d '{}')" 401
expect_eq "a made-up token is refused" "$(postcode -H 'authorization: Bearer aaa.bbb.ccc' -d '{}')" 401
INIT=$(post -H 'mcp-protocol-version: 2025-06-18' -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"rehearsal","version":"1"}}}')
expect_eq "legacy initialize names the server" "$(echo "$INIT" | jq_ "d['result']['serverInfo']['name']")" "dibibest"
expect_eq "a notification gets 202" "$(postcode -H "authorization: Bearer $TOKEN" -d '{"jsonrpc":"2.0","method":"notifications/initialized"}')" 202
expect_eq "tools/list lists three tools" "$(post -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | jq_ "len(d['result']['tools'])")" 3
CONV=$(post -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"convert_schema","arguments":{"source":"Table users {\n  id integer [pk]\n  email varchar [unique]\n}\n","to":"sql"}}}')
expect_has "convert_schema converts DBML to SQL" "$CONV" "CREATE TABLE users"
expect_has "...and reports fidelity" "$CONV" "-- fidelity --"
expect_has "list_formats lists the formats" "$(post -d '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_formats","arguments":{}}}')" "sql:mysql"
DISC=$(post -H 'mcp-protocol-version: 2026-07-28' -H 'mcp-method: server/discover' -d '{"jsonrpc":"2.0","id":1,"method":"server/discover"}')
expect_eq "modern server/discover" "$(echo "$DISC" | jq_ "d['result']['supportedVersions'][0]")" "2026-07-28"
MODERN=$(post -H 'mcp-protocol-version: 2026-07-28' -H 'mcp-method: tools/call' -H 'mcp-name: list_formats' -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_formats","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}')
expect_has "modern tools/call with the Mcp-Name header" "$MODERN" "sql:mysql"
expect_eq "a header that disagrees with the body is refused" "$(postcode -H "authorization: Bearer $TOKEN" -H 'mcp-protocol-version: 2026-07-28' -H 'mcp-method: tools/list' -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_formats","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}')" 400
PRM=$(curl -s "$BASE/.well-known/oauth-protected-resource/mcp")
expect_eq "protected resource metadata: the resource is the public address" "$(echo "$PRM" | jq_ "d['resource']")" "$AUD"
expect_eq "...and names the provider" "$(echo "$PRM" | jq_ "d['authorization_servers'][0]")" "$ISSUER"
ASM=$(curl -s "$BASE/.well-known/oauth-authorization-server/mcp")
expect_eq "the provider's own metadata is passed on" "$(echo "$ASM" | jq_ "d['issuer']")" "$ISSUER"
PRE=$(curl -s -D - -o /dev/null -X OPTIONS -H 'Origin: https://claude.ai' -H 'Access-Control-Request-Method: POST' "$AUD" | tr -d '\r')
expect_has "CORS preflight is answered" "$PRE" "204"
expect_has "...and echoes the origin" "$(echo "$PRE" | grep -i '^access-control-allow-origin')" "https://claude.ai"
LOGS=$(docker logs "$CONTAINER" 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
expect_has "the log has one line per call (a tools/list)" "$LOGS" 'mcp: status=200 method="tools/list"'
expect_has "...with the reason for a refusal" "$LOGS" 'reason="no bearer token"'
expect_not_has "...and no token ever reaches the log" "$LOGS" "$TOKEN"

echo "4. idle connections behind the proxy"
if [ "$FAST" = 1 ]; then
  echo "  skip  (--fast)"
else
  # The backend closes a connection after DIBIBEST_HEADER_TIMEOUT_SECS (120) without a request. Traefik keeps its own
  # idle connections for 90 s, so what is checked here is that a request after a pause longer than the old 10 s
  # default still goes through, three times.
  ok=0
  for i in 1 2 3; do
    sleep 12
    code=$(postcode -H "authorization: Bearer $TOKEN" -d '{"jsonrpc":"2.0","id":9,"method":"tools/list"}')
    [ "$code" = 200 ] && ok=$((ok + 1))
  done
  expect_eq "a POST after an idle pause of 12 s, three times" "$ok" 3
fi
# Any 502, 503 or 504 that Traefik logged since the checks began is a failure, except the 502 on /auth/callback that the
# SERVER sends on purpose when the provider refuses a code (section 2). A log that cannot be read is a failure too.
proxy_5xx_check() {
  local logs lines n
  if ! logs=$($COMPOSE logs --since "$E2E_START" traefik 2>&1); then fail "$1" "the Traefik log could not be read"; return; fi
  lines=$(printf '%s\n' "$logs" | grep -E '" 50[234] ' | grep -vE '/auth/callback.*" 502 ' || true)
  n=$(printf '%s' "$lines" | grep -c . || true)
  if [ "$n" = 0 ]; then pass "$1"; else fail "$1" "$(printf '%s' "$lines" | head -3 | cut -c1-240)"; fi
}
proxy_5xx_check "Traefik logged no 502, 503 or 504 since the checks began (idle pauses included)"

echo "5. load, the rate limit and a spoofed address"
codes=$(seq 1 150 | xargs -P 30 -I{} curl -s -o /dev/null -w '%{http_code}\n' "$BASE/" | sort | uniq -c | tr -s ' ' | tr '\n' ';')
expect_eq "150 parallel requests inside the budget all succeed" "$codes" " 150 200;"
codes=$(seq 1 500 | xargs -P 30 -I{} curl -s -o /dev/null -w '%{http_code}\n' "$BASE/" | sort | uniq -c | tr -s ' ' | tr '\n' ';')
expect_has "past the budget requests get 429" "$codes" "429"
# the bucket refills 5 tokens a second, so one request right after the burst could find a token by luck; thirty at
# once cannot all do so. If the made-up address gave a fresh budget, all thirty would pass.
SPOOF=$(seq 1 30 | xargs -P 30 -I{} curl -s -o /dev/null -w '%{http_code}\n' -H 'X-Forwarded-For: 9.9.9.9' "$BASE/" | grep -c 429 || true)
if [ "$SPOOF" -ge 20 ]; then pass "a client that writes its own X-Forwarded-For is limited all the same ($SPOOF of 30 refused)"; else fail "a client that writes its own X-Forwarded-For is limited all the same" "only $SPOOF of 30 refused"; fi
expect_eq "the probes are not limited" "$(status "$BASE/healthz")" 200
proxy_5xx_check "...and still none after the load"

echo "6. the backup"
LISTING=$(in_backups 'ls /b' 2>&1); LISTED=$?
if [ "$LISTED" -eq 0 ]; then pass "the backup directory can be listed"; else fail "the backup directory can be listed" "$LISTING"; fi
FIRST=$(echo "$LISTING" | grep '^dibibest-.*Z\.db$' | head -1)
if [ -n "$FIRST" ]; then pass "a backup was made at start: $FIRST"; else fail "a backup was made at start" "$LISTING"; fi
expect_eq "it is a SQLite database" "$(in_backups "head -c 15 /b/$FIRST" 2>/dev/null)" "SQLite format 3"
expect_eq "it is readable by its owner only (mode 600)" "$(in_backups "stat -c %a /b/$FIRST" 2>/dev/null)" 600
expect_eq "no half-written copy is left" "$(echo "$LISTING" | grep -c '\.partial$' || true)" 0

echo "7. the container"
expect_eq "it runs as the unprivileged user" "$(docker exec "$CONTAINER" id -u)" 10001
HEALTH=starting
for _ in $(seq 1 40); do
  HEALTH=$(docker inspect -f '{{.State.Health.Status}}' "$CONTAINER")
  [ "$HEALTH" = healthy ] && break
  sleep 1
done
expect_eq "its health check passes" "$HEALTH" healthy
CID=$(docker run -d -v dibibest-local-data:/data dibibest:local)
for _ in $(seq 1 20); do
  [ "$(docker inspect -f '{{.State.Running}}' "$CID")" = false ] && break
  sleep 1
done
LOCK=$(docker logs "$CID" 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
expect_eq "a second container on the same volume has stopped (it did not hang)" "$(docker inspect -f '{{.State.Running}}' "$CID")" false
expect_has "...because the data directory is in use" "$LOCK" "already using"
docker rm -f "$CID" >/dev/null 2>&1

echo "8. shutdown and restore"
T0=$(date +%s)
docker stop -t 30 "$CONTAINER" >/dev/null
T1=$(date +%s)
expect_eq "the server exits with 0 on SIGTERM" "$(docker inspect -f '{{.State.ExitCode}}' "$CONTAINER")" 0
if [ $((T1 - T0)) -lt 10 ]; then pass "...within $((T1 - T0)) s, well inside the grace period"; else fail "...within the grace period" "took $((T1 - T0)) s"; fi
expect_has "the log says it stopped" "$(docker logs "$CONTAINER" 2>&1 | sed 's/\x1b\[[0-9;]*m//g')" "stopped"
# the restore of docs/deploy.md, with this rehearsal's volume name and compose file, as one && chain
BK=$FIRST
docker run --rm -v dibibest-local-data:/data alpine:3.22 sh -ec '
  old="/data/before-restore-$(date -u +%Y%m%dT%H%M%SZ)"; mkdir "$old"
  for f in /data/dibibest.db /data/dibibest.db-wal /data/dibibest.db-shm; do
    if [ -e "$f" ]; then mv "$f" "$old"/; fi
  done' >/dev/null \
&& docker run --rm -v dibibest-local-data:/data -v "$PWD/backups":/backup:ro alpine:3.22 sh -ec "
  cp /backup/$BK /data/dibibest.db
  chown 10001:10001 /data/dibibest.db
  cmp /backup/$BK /data/dibibest.db" >/dev/null \
&& $COMPOSE start dibibest >/dev/null 2>&1 \
&& $COMPOSE restart idp >/dev/null 2>&1
RESTORED=0
for _ in $(seq 1 60); do
  if curl -sf "$BASE/readyz" >/dev/null 2>&1; then RESTORED=1; break; fi
  sleep 1
done
expect_eq "the server is healthy again on the restored database" "$RESTORED" 1
expect_has "...and /readyz still reports the WAL" "$(curl -s "$BASE/readyz")" '"wal":true'
for _ in $(seq 1 30); do [ "$(curl -s -o /dev/null -w '%{http_code}' "$IDP/api/auth/jwks")" = 200 ] && break; sleep 1; done
sleep 2
TOKEN=$(token "aud=$AUD")
expect_eq "...and the MCP endpoint answers a new token" "$(post -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | jq_ "len(d['result']['tools'])")" 3

echo
echo "passed $PASS, failed $FAIL"
if [ "$FAIL" -gt 0 ]; then
  printf 'failed: %s\n' "${FAILED[@]}"
  exit 1
fi

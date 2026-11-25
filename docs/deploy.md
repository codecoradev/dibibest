# Deploying the server (one container, one VPS)

The image built by the root `Dockerfile` holds the Rust server (`crates/dibibest-server`) and the built web app. It is
one container with one process. This is the plan for moving `dibibest.codecora.dev` from Cloudflare Pages
([deploy-web.md](deploy-web.md)) to a VPS; epic #84, issue #95. The example compose file is
[`docker-compose.yml`](../docker-compose.yml).

## What was run, and what was not

Run for real on 2026-11-24 (OrbStack, Docker 29, linux/arm64): the clean build, start on a new named volume, `/healthz`,
`/readyz`, `/`, a JSON 404 under `/v1`, path-traversal refusals, `docker stop` (exit 0), a second container on the same
volume (refused), restart on the same volume after `docker rm`, the non-root user, the health check, `docker compose
config`, `up` and `down`, and a restore of a cold copy of the volume (see Rollback).

Run for real on 2026-11-25 (the same machine): the scheduled backup (the first one at once, none extra after a restart, the
server refusing to start when the backup directory is read-only) and the restore commands under Backups, copied from this
file. The image was also **built** for `linux/amd64` (not run on an amd64 host).

**Not run:** the image on an amd64 host (the VPS is probably amd64), Traefik and Cloudflare in front, sign-in against Gatehouse, and any request with a real
project (the API for projects does not exist yet).

## Local rehearsal (run it before the VPS)

`deploy/local/run.sh` starts the production shape on one machine and tests it from outside, the way a client on the
internet would reach it:

- **Traefik** on `http://localhost:8088` in front (one proxy, so `DIBIBEST_TRUSTED_PROXIES=1`), routing to
- **the dibibest container**, built from the root `Dockerfile`, with the named volume `dibibest-local-data`, a host
  directory `deploy/local/backups` for backups, and the same variables as the VPS (made-up values; the differences from `docker-compose.yml` are listed at the top of
  `deploy/local/compose.yml`), next to
- **a fake identity provider** (`deploy/local/idp.mjs`, Node, no dependencies) that speaks just enough OpenID Connect
  for sign-in (discovery, authorization code with PKCE, token, userinfo, keys) and hands out ES256 access tokens for the
  MCP endpoint. It is not Gatehouse and proves nothing about Gatehouse.

`deploy/local/e2e.sh` then checks, through Traefik: the probes and the web app; the whole sign-in (login redirect with
PKCE, callback, session, a forged state, a replayed code, logout from another origin refused); the MCP endpoint with
real signed tokens (no token, wrong resource, wrong issuer, expired, made up; legacy and modern handshakes, `tools/list`,
`convert_schema` with its fidelity report, the `Mcp-Name` header rule, the metadata documents, CORS); that the log has one
line per call and never a token; that a POST after an idle pause still goes through and Traefik logs no 502; 150 parallel
requests, the rate limit, and that a client writing its own `X-Forwarded-For` is limited all the same; the backup (made at
start, a SQLite file, not readable by others); the container (unprivileged user, health check, a second container on the
volume refused); `docker stop` (exit 0, within the grace period); and the restore of the Backups section on this very
stack, after which the server is healthy again and answers MCP with a new token.

```sh
deploy/local/run.sh            # build, start, check, stop (a few minutes: the idle check waits 3 x 12 s)
deploy/local/run.sh --fast     # without the waits
deploy/local/run.sh --keep     # leave it running to try things by hand at http://localhost:8088
```

It needs Docker with compose, `curl` and `python3`, and the ports 8088 and 9000 on `127.0.0.1`. Run on 2026-11-25
(OrbStack, arm64): 62 checks, 62 passed.

What it does **not** show: the real Gatehouse (its metadata was fetched once, by hand, in the earlier checks of this
page), TLS and Cloudflare, a real Claude connector, a real amd64 host, a real VPS disk. The check for idle connections
shows that requests keep working after a pause; it does not provoke the race it guards against (a proxy sending a
request on a connection the server closes at that very moment), which is rare and cannot be forced from outside.

## Base image

The runtime image is `alpine:3.22` and the server is built on `rust:1.99-alpine3.22` for musl: the result is one static
binary (about 6 MB) and a 21 MB image, and busybox `wget` is already there for the health check, so nothing is
installed in the final stage. The binary was run on Alpine for real, with the bundled SQLite and `ring` both working
(the store opens in WAL mode, `/readyz` says `"wal":true`); the price is musl's allocator, which is slower than glibc's
under heavy multi-thread allocation, so measure before assuming it is fine when load becomes real.

## DNS and TLS

- `dibibest.codecora.dev` points today at Cloudflare Pages (`CNAME dibibest -> dibibest.pages.dev`, proxied). When the
  VPS takes over, change that record to the VPS (an `A`/`AAAA` record, proxied through Cloudflare, or a `CNAME` to the
  VPS's own name). Do it only after the container answers `/readyz` through the proxy, and keep the Pages project until
  the cutover has been checked with a real Claude connector (`/mcp` is ported to this server, see below), because the
  Pages Functions still serve the live site until then.
- **TLS ends at the reverse proxy.** The server speaks plain HTTP on 8080 and sets `Secure` cookies from
  `DIBIBEST_PUBLIC_URL`, not from the request. Traefik gets the certificate (its resolver, or a Cloudflare Origin
  certificate when Cloudflare proxies the name; then set Cloudflare's SSL mode to Full (strict)).
- The container publishes no port. Traefik reaches it over the Docker network named in `TRAEFIK_NETWORK`.

## Configuration

Everything is an environment variable; nothing is read from the repository or from a file. A blank value counts as
unset, a bad value stops the start with exit code 2 and names the variable. For `DIBIBEST_PUBLIC_URL` and the sign-in
variables, a value that is only a placeholder (`...`, `xxx`, `changeme`, `todo`) also counts as unset.

| Variable | Default in the server | Default in the image | Meaning |
|---|---|---|---|
| `DIBIBEST_ADDR` | `127.0.0.1:8080` | `0.0.0.0:8080` | Address and port to listen on. The health check follows the port. |
| `DIBIBEST_DATA_DIR` | `./data` | `/data` | Holds `dibibest.db` (SQLite, WAL) and `dibibest.lock`. Created if missing. Must be a named volume. |
| `DIBIBEST_WEB_DIR` | `./web` | `/app/web` | The built web app (`index.html`, `_app/`, ...). |
| `DIBIBEST_TRUSTED_PROXIES` | `0` | `0` | How many reverse proxies stand in front (0 to 16). See below. |
| `DIBIBEST_RATE_PER_MINUTE` | `600` | `600` | Requests per client per minute, also the burst size. At least 1. `/healthz` and `/readyz` are not limited. |
| `DIBIBEST_MAX_BODY_BYTES` | `1048576` | `1048576` | Largest request body (413 above it). |
| `DIBIBEST_SHUTDOWN_GRACE_SECS` | `20` | `20` | How long a shutdown waits for requests in flight. Keep Docker's `stop_grace_period` longer. |
| `DIBIBEST_HEADER_TIMEOUT_SECS` | `120` | `120` | A connection must deliver the headers of its next request within this time, or it is closed. It also ends an idle kept-alive connection, so it stays above the 90 s that Traefik keeps an idle upstream connection. 1 to 86400. |
| `DIBIBEST_REQUEST_TIMEOUT_SECS` | `30` | `30` | From the headers to the start of the response, body and handler together; over it the answer is 408. 1 to 86400. |
| `DIBIBEST_WRITE_TIMEOUT_SECS` | `60` | `60` | A write that makes no progress for this long ends the connection: a client that never reads a response bigger than the kernel buffers take, a slow but steady reader is not cut off. A client that never reads a small response never makes a write wait; it is ended by the header timeout once the connection is idle. 1 to 86400. |
| `DIBIBEST_MAX_CONNECTIONS` | `1024` | `1024` | Connections served at once; past it the server stops accepting and further ones wait in the operating system's queue. 1 to 1000000. |
| `DIBIBEST_MAX_CONNECTIONS_PER_IP` | `0` (off) | `0` | Connections one address (IPv6 by /64) may hold at once; the next is closed at once, with no response. Leave it at 0 behind a reverse proxy: every connection then comes from the proxy, so the server **refuses to start** with this above 0 and `DIBIBEST_TRUSTED_PROXIES` above 0. Limit per client at the proxy instead. 0 to 1000000. |
| `DIBIBEST_BACKUP_DIR` | unset | unset | Where scheduled backups are written. Unset: no backups. The compose file sets `/backup`. See Backups. |
| `DIBIBEST_BACKUP_INTERVAL_HOURS` | `24` | `24` | Hours between backups, 1 to 8760. Needs `DIBIBEST_BACKUP_DIR`. |
| `DIBIBEST_BACKUP_KEEP` | `7` | `7` | How many backups are kept, 1 to 1000. Needs `DIBIBEST_BACKUP_DIR`. |
| `DIBIBEST_PUBLIC_URL` | unset | unset | The origin people use, `https://dibibest.codecora.dev`: no path, query or credentials; `http` only for `localhost`. Needed when sign-in is on. |
| `DIBIBEST_OIDC_ISSUER` | unset | unset | Identity provider issuer, no query or fragment. For Gatehouse: `https://identity.azfirazka.com/api/auth` (the value `apps/web/wrangler.toml` uses today). |
| `DIBIBEST_OIDC_CLIENT_ID` | unset | unset | The Gatehouse client id. |
| `DIBIBEST_OIDC_CLIENT_SECRET` | unset | unset | The Gatehouse client secret. **Secret.** |
| `DIBIBEST_SESSION_SECRET` | unset | unset | Signs the login handshake and the session cookie; at least 32 characters (`openssl rand -base64 32`). **Secret.** |
| `DIBIBEST_OIDC_TOKEN_AUTH` | `basic` | `basic` | `basic` (`client_secret_basic`) or `post` (`client_secret_post`). |
| `RUST_LOG` | `info` | `info` | Log filter (not a `DIBIBEST_` variable). |

Sign-in is **all or nothing**: the client id, client secret and session secret are set together with the issuer, or none
of them. A half is a start-up error, and so is any identity provider setting without `DIBIBEST_PUBLIC_URL`. The issuer
on its own (with the public URL) is allowed and turns on the remote MCP endpoint (`/mcp` and the `/.well-known/*`
documents) without sign-in. With none set the server runs without either. Put the secrets in the `.env` next to the
compose file, never in the compose file or the repository; this repository's `.gitignore` lists `.env`, but do not
`git add -f` it.

### Gatehouse client

Confidential client with PKCE, scopes `openid email profile`, and the redirect URI **unchanged**:
`https://dibibest.codecora.dev/auth/callback`. The server builds that address from `DIBIBEST_PUBLIC_URL`, so the two
must agree exactly (scheme, host, no trailing slash). The `https://dibibest.pages.dev/auth/callback` entry can stay
while Pages is still live and be removed afterwards. If the client is registered for `client_secret_post`, set
`DIBIBEST_OIDC_TOKEN_AUTH=post`. Whether people stay signed in when the DNS moves from Pages to this container was not
checked; plan for a fresh sign-in.

### `DIBIBEST_TRUSTED_PROXIES`

The rate limiter needs the real client address. With N, the client is the Nth entry from the end of `X-Forwarded-For`
(every header line counts); the proxies wrote those entries and the client wrote the rest, which is ignored. At the
default `0` the header is ignored altogether, which is right with no proxy and wrong behind one (every request then
looks like it comes from the proxy and shares one bucket).

| In front of the container | Value |
|---|---|
| Nothing (a port published on loopback for testing) | `0` |
| Traefik only | `1` |
| Cloudflare, then Traefik | `2` (the example compose file sets this) |

A wrong value in either direction weakens the limit: too low and clients share a bucket; too high and the client can
choose its own address. Check it by looking at the rate-limit behaviour after the first deploy (not tested here).

## The data volume

`/data` is a Docker **named volume** (`dibibest-data` in the example). **Never a bind mount.** SQLite in WAL mode on a
Docker Desktop (macOS, VirtioFS) bind mount lost data in another project of ours; the same fear applies to network
file systems. The image creates `/data` owned by the unprivileged user `dibibest` (uid/gid 10001) and declares it a
`VOLUME`, so a new named volume is writable at once (checked: `touch /data/x` works on a fresh volume, and the server
created the database and the lock on it). A bind-mounted directory is not initialised that way and would need its owner
set by hand; that is one more reason not to use one.

## One instance at a time

The server keeps room state in memory and the store has one writer, so exactly one process may use a data directory.
It takes an exclusive lock on `/data/dibibest.lock` before it opens the database; a second process prints
`cannot start: another dibibest-server is already using /data` and exits with code 1 without touching the database
(checked, two containers on one volume). The lock goes away with the process, so a crash leaves nothing to clean up.

So a deploy is **stop the old container, then start the new one**: `docker compose pull && docker compose up -d`
(or `up -d --build`) recreates the container in that order. Never `--scale`, never a "start first" rolling update.
Expect a short gap in service; `docker stop` took well under a second when idle (SIGTERM, graceful, exit code 0).

## Health

The image has a `HEALTHCHECK` on `/healthz` (liveness). `/readyz` is readiness (the store answers, WAL is on, the disk is
not full); the compose file does not gate on it, but a monitor should watch it. The container was `healthy` when checked
about 15 to 20 seconds after the start (interval 15 s, start period 10 s), under plain `docker run` and under compose.

With plain `docker run` (no compose), pass `--stop-timeout 30`: Docker's default of 10 seconds kills the process before the
20-second shutdown grace is over, and a request in flight is cut off.

## The image from CI (GHCR)

`.github/workflows/image.yml` builds the image on every push to `develop` that touches what goes into it (the
`Dockerfile`, the crates, the web app, the lock file) and can push it to GitHub's registry. It never runs for a pull
request, and it does not touch the VPS: the VPS pulls by hand.

- **The build always runs** (for `linux/amd64`, or what the repository variable `IMAGE_PLATFORMS` says), so a merge that
  breaks the image shows up on `develop`.
- **The push is off until the repository variable `GHCR_PUSH` is `true`.** Set it under Settings > Secrets and variables
  > Actions > Variables. The first push is the owner's call.
- **Tags:** `develop` (moves with every push) and `sha-<seven characters>` (fixed). There is no `latest`: name a tag in
  the compose file (`image: ghcr.io/codecoradev/dibibest:sha-abc1234`), and a rollback is the previous tag.
- **Public or private package:** a new GHCR package is private. For a private one the VPS needs, once,
  `docker login ghcr.io -u <user>` with a token that can read packages; for a public one it needs nothing. Change it
  under the package's settings on GitHub.
- **On the VPS, to update:** in the compose file replace `build: .` with `image: ghcr.io/codecoradev/dibibest:<tag>` (once;
  with `build: .` there is nothing to pull), then `docker compose pull && docker compose up -d` (this recreates the
  container: stop, then start). Every later update changes the tag.

## Rollback

1. Tag every image you deploy (`image: ...:<tag>`, not `latest`), and keep the previous tag.
2. To roll back, set the previous tag in the compose file and `docker compose up -d` (stop first, as above).
3. The volume is not rolled back with the image. Whether an older server can read a database written by a newer one
   depends on the schema changes between them, and there is no down-migration; before a release that changes the store,
   take a backup first (next section).

## Backups

The server makes the backups itself, from inside the running process (nothing to schedule on the host, no second
container). Each one is a consistent copy made by the store (`VACUUM INTO`), never a copy of the live `.db` file, which
would miss what is still in the `-wal` file.

- **Switch on** by giving the server a directory: `DIBIBEST_BACKUP_DIR` (the compose file sets `/backup` and mounts
  the host directory `./backups` there). Without it there are no backups.
- **Where:** a host directory bind-mounted into the container, **not the data volume**: a backup on the volume it
  protects is lost with it. The database itself stays on the named volume `dibibest-data` (SQLite in WAL mode lost data
  on a Docker Desktop bind mount in another project). The server runs as uid 10001, so make the directory writable for it
  once: `mkdir -m 700 backups && sudo chown 10001:10001 backups`. The server itself closes the directory to everybody else
  (`0700`) and each backup to its owner (`0600`), since a backup holds every project in the database; as the files belong
  to uid 10001, read them on the host with `sudo`, or `docker run` as in Restore. If the server cannot change the modes
  (a directory owned by somebody else, a network share that ignores them), it logs `cannot restrict ... to its owner` on
  every backup and carries on: the copies are then only as private as that directory is. If the directory cannot be written to the server **refuses to
  start** and says so, because a server that looks fine and never backs up is worse.
- **When:** every `DIBIBEST_BACKUP_INTERVAL_HOURS` (default 24; 1 to 8760). Restarting does not make an extra one: the
  first backup waits for what is left of the interval since the newest one, or is made at once if there is none or the
  newest is older than the interval.
- **How many:** the newest `DIBIBEST_BACKUP_KEEP` (default 7; 1 to 1000) are kept, older ones are deleted. Only files named
  like a backup are ever touched.
- **Names:** `dibibest-YYYYMMDDTHHMMSSZ.db` (UTC). A copy is written as `<name>.partial`, flushed to disk and then
  renamed, so a copy cut short by a crash or a full disk is never taken for a whole one; leftovers are removed by the
  next run.
- **Failures** are logged (`backup failed, trying again soon`) and tried again after at most 15 minutes. Watch the log, or
  the age of the newest file: nothing else reports a failed backup yet.
- **Not covered:** off-site copies. The backup directory is on the same host (and maybe the same disk) as the data: copy
  it elsewhere yourself.

### Restore

A backup file is a complete database. To use one, stop the server, move the current database aside, put the backup in
its place, start the server again. Replace the file name in the second `docker run` with the backup you want
(`sudo ls backups`: the directory is closed to other users).

The steps are joined with `&&` on purpose: if one fails, the next does not run. In particular a failed copy must not be
followed by starting the server, which would create a new, empty database. The first `docker run` moves the current
database and its `-wal` and `-shm` files into a new directory on the volume (nothing is deleted); the second puts the
backup in place.

```sh
docker compose stop dibibest \
&& docker run --rm -v dibibest-data:/data alpine:3.22 sh -ec '
  old="/data/before-restore-$(date -u +%Y%m%dT%H%M%SZ)"; mkdir "$old"
  for f in /data/dibibest.db /data/dibibest.db-wal /data/dibibest.db-shm; do
    if [ -e "$f" ]; then mv "$f" "$old"/; fi
  done
  ls -l "$old"' \
&& docker run --rm -v dibibest-data:/data -v "$PWD/backups":/backup:ro alpine:3.22 sh -ec '
  cp /backup/dibibest-20261124T082320Z.db /data/dibibest.db
  chown 10001:10001 /data/dibibest.db
  ls -l /data/dibibest.db' \
&& docker compose start dibibest \
&& docker compose exec dibibest wget -qO- http://127.0.0.1:8080/readyz
```

If a step fails, the server is still stopped and the old database is where the first step left it (in the
`before-restore-<time>` directory if that step ran): move it back and start the server.

The compose file names the volume `dibibest-data` explicitly, so these commands mount the volume the service uses. Each
restore keeps its own `before-restore-<time>` directory on the volume; delete them when you no longer need them.
The commands above were run as written against a real container (see the top of this page), and `tests/backup.rs`
compares a restored copy with the live database: the log and the snapshot are equal.

Until the server has a projects API there is little in the database to lose, but the routine is the same one you will
need later.

## What does not exist yet

- **No off-site backup and no alert for a failed backup** (see Backups): the copies stay on the host and a failure is only in the log.
- **`/mcp` is served by this container but has not met a real connector yet.** The Rust port (issue #94) passes the 55
  shared contract fixtures and its own edge-case tests, but it was never run behind Traefik and Cloudflare, with
  Gatehouse and a real Claude client. It needs `DIBIBEST_OIDC_ISSUER` and `DIBIBEST_PUBLIC_URL`; without them `/mcp` and
  the `/.well-known/*` documents answer 404 "remote MCP is not configured". The Cloudflare Pages Functions keep serving
  the live site until the DNS moves, so do the connector check on the temporary hostname first.
- **No projects API yet:** `/v1/...` answers a JSON 404 and the live (WebSocket) part is empty. The container serves the
  web app, the probes and the sign-in routes; the database is created and opened but nothing writes to it yet.
- **Sign-in routes** (`/auth/me`, `/login`, `/callback`, `/logout`) exist in the server but were not exercised through
  this image against Gatehouse.
- **No published image.** The compose file builds from source. A registry and a release workflow are not set up.
- **No metrics and no log shipping.** Logs go to stdout (`docker compose logs`).
- **Only one architecture was built** (arm64, on the machine that wrote this).

## Build notes

- `docker build -t dibibest .` from a clean cache took 5 min 11 s on an Apple Silicon machine (rust dependencies are the
  bulk). With a warm cache: nothing changed 4 s; only web sources changed about 15 s; a server source change about 2
  minutes (only the `dibibest-store` and `dibibest-server` crates recompile; the release profile has fat LTO, so the
  link is the long part).
- The dependency layer uses empty "dummy" crates, not `cargo-chef`: no extra tool to download or trust. If a crate
  declares a new target (`[[test]]`, a build script), the stubs in the `Dockerfile` may need to follow.
- bun is downloaded as a release zip and checked against a SHA-256 in the `Dockerfile`; bump the version and both
  hashes together.
- `.dockerignore` keeps `.git`, `target`, `node_modules`, the web build, docs and any `.env` out of the build context
  (it is also used by `Dockerfile.mcp`).

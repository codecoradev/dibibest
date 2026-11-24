# syntax=docker/dockerfile:1
#
# dibibest server image: ONE container holding the Rust server and the built web app.
#
#   docker build -t dibibest .
#   docker run --rm -p 8080:8080 -v dibibest-data:/data dibibest
#
# Stages:
#   web      builds the WASM core, then the static web app (apps/web -> build/)
#   server   builds `dibibest-server` for musl (SQLite is bundled and compiled from C)
#   runtime  Alpine with the binary, the web build and nothing else
#
# Deploy notes (named volume, one instance at a time, variables): docs/deploy.md.

ARG RUST_IMAGE=rust:1.99-alpine3.22
ARG RUNTIME_IMAGE=alpine:3.22

# ---------------------------------------------------------------------------------------------------------------------
# Stage 1: the web app
# ---------------------------------------------------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS web

# bun (the web app's package manager and build runner) is not in the Rust image, so it is downloaded as a release
# zip and checked against a SHA-256 written down here. The hashes are the ones in the release's SHASUMS256.txt for
# bun-v1.4.4. They catch a corrupted or substituted download; they do not make the release itself trustworthy.
# Bump BUN_VERSION and both hashes together.
ARG BUN_VERSION=1.4.4
ARG BUN_SHA256_AMD64=a5a8ccc5e74dcb883fe974f125a7bdb3b41e5195ed1b870e73f17effd3eb2b2d
ARG BUN_SHA256_ARM64=57f01b02ac7113c61c935f28f38c2eab849e3c970dab21f8b640a87b1ef65009
ARG TARGETARCH

# The musl build of bun needs libstdc++ and libgcc at run time.
RUN apk add --no-cache curl unzip libstdc++ libgcc \
 && case "${TARGETARCH:-$(uname -m)}" in \
      amd64|x86_64)  asset=bun-linux-x64-musl;     sha="${BUN_SHA256_AMD64}" ;; \
      arm64|aarch64) asset=bun-linux-aarch64-musl; sha="${BUN_SHA256_ARM64}" ;; \
      *) echo "unsupported architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && curl -fsSL -o /tmp/bun.zip "https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/${asset}.zip" \
 && echo "${sha}  /tmp/bun.zip" | sha256sum -c - \
 && unzip -q /tmp/bun.zip -d /tmp/bun \
 && install -m 0755 "/tmp/bun/${asset}/bun" /usr/local/bin/bun \
 && rm -rf /tmp/bun /tmp/bun.zip \
 && rustup target add wasm32-unknown-unknown \
 && bun --version

WORKDIR /src

# Dependencies first: this layer only changes with package.json or bun.lock.
COPY apps/web/package.json apps/web/bun.lock apps/web/
RUN cd apps/web && bun install --frozen-lockfile

# The WASM core, in its own layer, so a change to the web sources does not rebuild it. The workspace manifest lists
# every crate, so the crates the WASM bridge does not use are replaced by empty stubs: a change to the server or the
# store then leaves this layer alone. (The stubs make cargo prune them from the copy of Cargo.lock inside the image;
# the real Cargo.lock is not touched.)
COPY Cargo.toml Cargo.lock ./
COPY crates/dibibest-core crates/dibibest-core
COPY crates/dibibest-wasm crates/dibibest-wasm
RUN for c in cli mcp store server; do \
      mkdir -p "crates/dibibest-$c/src" \
   && : > "crates/dibibest-$c/src/lib.rs" \
   && printf '[package]\nname = "dibibest-%s"\nversion = "0.1.0"\nedition = "2021"\n' "$c" > "crates/dibibest-$c/Cargo.toml"; \
    done
COPY apps/web/scripts apps/web/scripts
RUN mkdir -p apps/web/src/lib && sh apps/web/scripts/wasm.sh

# The static site. `bun run build` runs scripts/wasm.sh again, which finds the WASM core up to date and copies it.
COPY apps/web apps/web
RUN cd apps/web && bun run build \
 && test -f build/index.html

# ---------------------------------------------------------------------------------------------------------------------
# Stage 2: the server
# ---------------------------------------------------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS server

# gcc and musl-dev: bundled SQLite (rusqlite) and ring are compiled from C.
RUN apk add --no-cache gcc musl-dev

WORKDIR /src

# Dependency layer (the "dummy manifest" way, no extra tool): copy only the manifests, give every crate an empty
# source file, and build the server once. That compiles all third-party dependencies and caches them in this layer,
# which only changes when a Cargo.toml or Cargo.lock does. The crates are listed one by one because a COPY of
# crates/*/Cargo.toml would flatten them into one directory.
COPY Cargo.toml Cargo.lock ./
COPY crates/dibibest-core/Cargo.toml crates/dibibest-core/Cargo.toml
COPY crates/dibibest-cli/Cargo.toml crates/dibibest-cli/Cargo.toml
COPY crates/dibibest-mcp/Cargo.toml crates/dibibest-mcp/Cargo.toml
COPY crates/dibibest-wasm/Cargo.toml crates/dibibest-wasm/Cargo.toml
COPY crates/dibibest-store/Cargo.toml crates/dibibest-store/Cargo.toml
COPY crates/dibibest-server/Cargo.toml crates/dibibest-server/Cargo.toml
RUN for d in crates/*/; do \
      mkdir -p "$d/src" && : > "$d/src/lib.rs" && echo 'fn main() {}' > "$d/src/main.rs"; \
    done \
 && cargo build --release --locked -p dibibest-server

# The real sources. COPY keeps the files' own modification times, which can be older than the stubs above, so they
# are touched: cargo decides what to rebuild by modification time.
COPY crates crates
RUN find crates -name '*.rs' -exec touch {} + \
 && cargo build --release --locked -p dibibest-server \
 && cp target/release/dibibest-server /dibibest-server

# ---------------------------------------------------------------------------------------------------------------------
# Stage 3: the image that runs
# ---------------------------------------------------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS runtime

LABEL org.opencontainers.image.title="dibibest-server" \
      org.opencontainers.image.description="dibibest server: web app, API and live editing behind one origin" \
      org.opencontainers.image.source="https://github.com/codecoradev/dibibest" \
      org.opencontainers.image.licenses="MIT"

# A fixed, unprivileged user with no login shell. The data directory is created here and owned by it, so a new
# named volume (which Docker fills from this directory the first time it is mounted) is writable.
RUN addgroup -S -g 10001 dibibest \
 && adduser -S -D -H -u 10001 -G dibibest -h /nonexistent -s /sbin/nologin dibibest \
 && mkdir -p /data /app/web \
 && chown dibibest:dibibest /data

COPY --from=server /dibibest-server /usr/local/bin/dibibest-server
COPY --from=web /src/apps/web/build /app/web

# Defaults for the container. Everything else (DIBIBEST_PUBLIC_URL, DIBIBEST_OIDC_*, DIBIBEST_SESSION_SECRET,
# DIBIBEST_TRUSTED_PROXIES, ...) is passed at run time; see docs/deploy.md.
ENV DIBIBEST_ADDR=0.0.0.0:8080 \
    DIBIBEST_DATA_DIR=/data \
    DIBIBEST_WEB_DIR=/app/web

USER dibibest:dibibest
WORKDIR /data

# One server process per data directory (it holds an exclusive lock there). Keep this a named volume.
VOLUME /data
EXPOSE 8080

# Exec form, no shell: the server is PID 1 and receives SIGTERM itself, finishes its requests, flushes the store
# and exits 0.
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/dibibest-server"]

# Liveness. busybox wget exits non-zero on a connection error or an HTTP error status. The port follows
# DIBIBEST_ADDR, so changing the port does not break the check.
HEALTHCHECK --interval=15s --timeout=3s --start-period=10s --retries=3 \
  CMD wget -q -O /dev/null "http://127.0.0.1:${DIBIBEST_ADDR##*:}/healthz" || exit 1

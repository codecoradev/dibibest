# Auth & saved projects

`crates/dibibest-api` is a small axum server: an OIDC relying party for
Gatehouse ("Sign in with CodeCora ID") plus SQLite storage for saved projects.
The web app stays a static SPA; it never holds a client secret. Gatehouse offers
no `none` token-auth method, so the code exchange has to happen server-side.

The feature ships **OFF**. With no configuration the server starts, answers
`GET /auth/oidc/status` with `{"enabled":false}`, and every other route returns
a JSON 404.

## Setup

1. In the Gatehouse admin (`/admin/oauth2`) register a client with the exact
   redirect URI `<PUBLIC_URL>/auth/callback` (https in production). Copy the
   client secret; it is shown once.
2. Set the environment (never commit these):

   | Variable | Required | Meaning |
   |---|---|---|
   | `OIDC_ISSUER` | yes | Issuer URL; discovery is fetched from `<issuer>/.well-known/openid-configuration` |
   | `OIDC_CLIENT_ID` | yes | Client id from the registration |
   | `OIDC_CLIENT_SECRET` | yes | Client secret (sent with `client_secret_post`) |
   | `PUBLIC_URL` | yes | Public origin of this server, no trailing path |
   | `SESSION_SECRET` | yes | At least 32 bytes; generate with `openssl rand -base64 32`. Shorter counts as not set (feature OFF, warning on stderr). Hashed to 32 bytes for HMAC-SHA256 |
   | `ALLOWED_EMAILS` | no | Comma-separated allowlist |
   | `DIBIBEST_API_DB` | no | SQLite path (default `./dibibest.db`) |
   | `DIBIBEST_API_ADDR` | no | Listen address (default `127.0.0.1:8787`) |

   A variable that is empty or a placeholder (`...`, `xxx`, `changeme`) counts as
   not set. The feature turns on only when all five required variables are real.
3. Run: `cargo run -p dibibest-api --release`.

## Flow

```
browser            dibibest-api                         Gatehouse (OP)
   | GET /auth/login      |                                   |
   |--------------------->| GET discovery ------------------->|
   |                      | mint state + PKCE verifier        |
   |<-- 303 authorize URL + signed 10-min pkce cookie ---------|
   | authorize (S256 challenge) ----------------------------->|
   |<-- 303 /auth/callback?code&state -------------------------|
   | GET /auth/callback   |                                   |
   |--------------------->| verify state (pkce cookie)        |
   |                      | POST token: code + verifier +     |
   |                      |   client_secret_post ------------>|
   |                      | GET userinfo (Bearer) ----------->|
   |                      | allowlist check on email          |
   |<-- 303 /app + session cookie                              |
```

## Session

`dibibest_session` = `base64url(json{email,exp}).base64url(HMAC-SHA256)`,
valid 7 days, `HttpOnly; SameSite=Lax; Path=/` (plus `Secure` when `PUBLIC_URL`
is https). Signatures are compared in constant time. There is no refresh-token
handling: after 7 days the user signs in again.

## Allowlist

`ALLOWED_EMAILS` empty: any identity the OP authenticates may sign in.
Non-empty: the userinfo email must match an entry, case-insensitively. The
list is re-checked on **every** authenticated request, so a user removed from
it loses access on their next request. A userinfo `email_verified: false` is
denied too (an absent claim is allowed). A denied
user sees a "Not authorized" page (HTTP 403) with a log-out button and gets
**no** session cookie.

## Logout

`POST /auth/logout` clears the session cookie and redirects to `/`. It does not
end the Gatehouse session (no RP-initiated logout yet).

## Endpoints

| Route | Purpose |
|---|---|
| `GET /auth/oidc/status` | `{"enabled":bool}` |
| `GET /auth/login`, `GET /auth/callback`, `POST /auth/logout` | OIDC flow |
| `GET /` | Landing card, or 303 to `/app` when signed in |
| `GET/POST /api/projects` | List summaries / create `{name, erdjson}` |
| `GET/PUT/DELETE /api/projects/{id}` | Read / replace / delete |

All `/api/projects` routes require a valid session and are scoped to the
session's email; another user's project id returns 404. Errors are JSON
`{"error": "..."}`.

## Limits

- 512 KB of ERD-JSON per project (413 above that)
- 100 projects per user (409 when full)
- name 1–200 characters (400 otherwise)

## Known tradeoffs

- **Stateless sessions.** Signed 7-day cookies; logout clears the cookie only.
  Server-side revocation = rotate `SESSION_SECRET` (signs everyone out). A
  per-boot instance ID was considered and rejected for v1 simplicity.
- **Synchronous SQLite.** One connection behind a mutex, used directly on async
  workers. Adequate for a single instance at small-team scale; `spawn_blocking`
  or a connection pool is the upgrade path.

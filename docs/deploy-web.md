# Web deploy and sign-in (Cloudflare Pages)

The web app (`apps/web`) runs on Cloudflare Pages: static files for the
workspace (conversion runs in the browser through the WASM core) plus a few
Pages Functions for sign-in. There is no database: projects live in the
browser (IndexedDB) and in exported files, and the session is a signed cookie.
Background and decisions: epic #52.

## What is deployed

| Path | What |
|---|---|
| `/`, `/app` | Static SPA (adapter-static, `build/`) |
| `GET /auth/me` | `{"enabled": bool, "user": {sub, email, name?} \| null}` |
| `GET /auth/login?return_to=/app` | Redirect to CodeCora ID (Gatehouse), PKCE S256 + state |
| `GET /auth/callback` | Code exchange (confidential client), userinfo, session cookie |
| `POST /auth/logout` | Clears the session (same-origin only) |
| `POST /mcp` | Remote MCP server (Streamable HTTP), OAuth required; see below |
| `GET /.well-known/oauth-protected-resource[/mcp]` | RFC 9728 metadata pointing MCP clients at Gatehouse |

The route files are in `apps/web/functions/`; the logic and its unit
tests are in `apps/web/server/` (`bun run test:server`).

**Sign-in gates import, copy and download** (owner decision 2026-11-05), even
though projects stay in the browser. Editing, converting, the diagram, the
fidelity report and local projects work without an account. The gate is a UI funnel for usage
tracking (sign-ins are logged per client by Gatehouse), not a security
boundary: conversion runs in the browser.

## Deploy

`.github/workflows/deploy-web.yml` runs on every push to `develop` (and on
demand). It:

1. builds the web app (including the WASM core);
2. creates the Pages project `dibibest` if it does not exist (production
   branch `develop` until a `main` branch exists);
3. syncs the sign-in secrets when they are set (below);
4. runs `wrangler pages deploy`;
5. attaches `dibibest.codecora.dev` (warning only if the token cannot).

## Owner checklist

1. **Cloudflare credentials:** give this repository access to the org secrets
   `CLOUDFLARE_API_TOKEN` (Pages: Edit; plus DNS edit on `codecora.dev` for
   the custom domain) and `CLOUDFLARE_ACCOUNT_ID`.
2. **Custom domain:** the workflow registers `dibibest.codecora.dev` on the
   Pages project, but registering through the API does **not** create the DNS
   record (the dashboard does). Add it once in the `codecora.dev` zone:
   `CNAME dibibest → dibibest.pages.dev`, proxied. Without it the
   `*.codecora.dev` wildcard catches the name and answers 522; the domain then
   turns Active under Workers & Pages → dibibest → Custom domains.
3. **Gatehouse client** (CodeCora ID admin → OAuth clients): a confidential
   client with PKCE and the redirect URIs
   - `https://dibibest.codecora.dev/auth/callback`
   - `https://dibibest.pages.dev/auth/callback`

   Scopes: `openid email profile`.
4. **Configuration:**
   - put the client id in `apps/web/wrangler.toml` (`OIDC_CLIENT_ID`, public);
   - add the repo secrets `DIBIBEST_OIDC_CLIENT_SECRET` (from Gatehouse) and
     `DIBIBEST_SESSION_SECRET` (at least 32 characters, e.g.
     `openssl rand -base64 32`). The next deploy pushes both to Pages.

   Until all of this is set, `/auth/me` answers `{"enabled": false}` and the app
   keeps import/export open.

The token endpoint is called with `client_secret_basic`. If Gatehouse is
configured for `client_secret_post` instead, set `OIDC_TOKEN_AUTH = "post"` in
`[vars]`.

## Remote MCP (`/mcp`)

The same tools as the local stdio server (`convert_schema`, `render_diagram`,
`list_formats`),
running the core compiled to WASM inside a Pages Function. Client config:

```json
{ "mcpServers": { "dibibest": { "url": "https://dibibest.codecora.dev/mcp" } } }
```

- **OAuth only, no API keys.** A request without a valid token gets `401` with
  `WWW-Authenticate: Bearer resource_metadata="…/.well-known/oauth-protected-resource/mcp"`.
  The metadata names Gatehouse as the authorization server; the client signs
  the user in there (PKCE, Client ID Metadata Document for registration) and
  asks for `resource=https://dibibest.codecora.dev/mcp` (RFC 8707).
- **Tokens are verified locally:** an RFC 9068 JWT signed by a key in
  Gatehouse's JWKS (RS256/PS256/ES256, cached for an hour, refetched on an
  unknown `kid`), with `iss` = the issuer, `aud` containing this origin's
  `/mcp` URL, valid `exp`/`nbf`, a `sub`, and no `cnf` (DPoP-bound tokens are
  refused because the proof is not checked). Opaque tokens are refused.
- **Transport:** stateless, JSON responses only (no SSE stream, no
  `Mcp-Session-Id`); `GET` answers 405. Protocol versions 2025-11-25,
  2025-06-18 and 2025-03-26, negotiated in `initialize`. The
  `MCP-Protocol-Version` header is not gated. Claude opens with
  `server/discover` (MCP 2026-07-28), gets JSON-RPC -32601 with HTTP 200, and
  falls back to `initialize`. Browser origins get CORS (claude.ai sends
  `Origin: https://claude.ai`). The origin is echoed, `WWW-Authenticate` is
  exposed, and `OPTIONS` preflight is answered. This is safe because every
  request needs a bearer token and no cookie is read. Bodies over 512 KB get
  413.
- **Logs:** each call logs `mcp: <method>`, and refused tokens log the reason
  (never the token). See Workers & Pages → dibibest → Functions → Real-time
  logs.
- **Enabled** whenever `OIDC_ISSUER` is set (no client secret is needed: this
  is a resource server). It answers 401 to everyone until Gatehouse issues
  tokens for the resource (owner step below).

### Gatehouse setup for MCP (owner)

Gatehouse (`identity.azfirazka.com`) issues a JWT access token for `/mcp` only
when the URL is a configured resource. Otherwise the authorize step answers
`invalid_target` ("requested resource … is not configured"), and Claude shows
"Authorization with dibibest failed".

1. Add the MCP URL to the Gatehouse env **`OAUTH_RESOURCES`** (exact match,
   next to the existing entries such as `https://uteke.app/mcp`), then restart
   Gatehouse. Done 2026-11-06 for `https://dibibest.codecora.dev/mcp`. Add
   `https://dibibest.pages.dev/mcp` too only if that hostname is used.
2. Clients register by **CIMD**. Gatehouse runs `CIMD_ENABLED` with
   `CIMD_ALLOWED_ORIGINS=https://claude.ai`, which covers claude.ai, Desktop,
   mobile and Claude Code, so no client ID, secret or API key is needed. Other
   clients (ChatGPT, Cursor, …) need their metadata-document origin allowed.
3. Tokens: with `resource=<MCP URL>` the token is an RS256 JWT with
   `aud = [resource, userinfo]` and `azp = client_id`. Without `resource` it is
   opaque, and `/mcp` refuses it. Tokens carry no email (the endpoint only
   needs `sub`). They live one hour and cannot be revoked early.

Check after a change. With Claude's CIMD client id, the authorize step must
redirect to `identity.azfirazka.com/login`, not back with `invalid_target`:

```bash
curl -s -o /dev/null -w '%{redirect_url}\n' "https://identity.azfirazka.com/api/auth/oauth2/authorize?response_type=code&client_id=https%3A%2F%2Fclaude.ai%2Foauth%2Fmcp-oauth-client-metadata&redirect_uri=https%3A%2F%2Fclaude.ai%2Fapi%2Fmcp%2Fauth_callback&scope=openid&state=s&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&resource=https%3A%2F%2Fdibibest.codecora.dev%2Fmcp"
```

Connect in Claude with **Settings → Connectors → Add custom connector**, URL
`https://dibibest.codecora.dev/mcp`, then sign in with CodeCora ID.

## Limits and cost (Cloudflare, checked 2026-11-05)

- Static files (the whole workspace and conversions) are free and unlimited.
- Pages Functions count as Workers requests: Free plan 100,000 per day and
  10 ms CPU per request; Workers Paid ($5/month per account) includes
  10 million requests and 30 million CPU-ms per month, then metered. There is
  no flat unlimited plan below Enterprise.
- Sign-in uses a handful of function requests per login.
- Each MCP request is one function request. Token checks use Web Crypto and
  the core runs as a precompiled WASM module, so a typical conversion stays
  within the Free plan's 10 ms CPU; very large schemas may not, in which case
  Workers Paid (30 s CPU) is the fix.

## Local check

`wrangler pages dev` in `apps/web` (after `bun run build`) serves the build and
the Functions on workerd. Pass `--binding OIDC_ISSUER=… --binding
OIDC_CLIENT_ID=… --binding OIDC_CLIENT_SECRET=… --binding SESSION_SECRET=…`
to try sign-in against a real or fake provider.

## Session

`dibibest_session` = `base64url(json{sub, email, name, exp}).base64url(HMAC-SHA256)`,
valid 7 days, `HttpOnly; SameSite=Lax; Path=/` (plus `Secure` on https). The
login handshake (`dibibest_login`, path `/auth`, 10 minutes) carries `state`,
the PKCE verifier and `return_to` (same-site paths only). Rotating
`SESSION_SECRET` signs everyone out.

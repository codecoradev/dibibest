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

The route files are in `apps/web/functions/auth/`; the logic and its unit
tests are in `apps/web/server/` (`bun run test:server`).

**Sign-in gates only import and download.** Editing, converting, copying and
local projects work without an account. The gate is a UI funnel for usage
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
2. **Custom domain:** `codecora.dev` must be a zone in the same Cloudflare
   account; the workflow then attaches `dibibest.codecora.dev` and Cloudflare
   creates the CNAME. Otherwise add it once under Workers & Pages → dibibest →
   Custom domains.
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

## Limits and cost (Cloudflare, checked 2026-11-05)

- Static files (the whole workspace and conversions) are free and unlimited.
- Pages Functions count as Workers requests: Free plan 100,000 per day and
  10 ms CPU per request; Workers Paid ($5/month per account) includes
  10 million requests and 30 million CPU-ms per month, then metered. There is
  no flat unlimited plan below Enterprise.
- Sign-in uses a handful of function requests per login.

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

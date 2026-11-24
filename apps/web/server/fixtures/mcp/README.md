# MCP contract fixtures

One JSON file is one case of the remote MCP's behaviour: a request, or a short sequence of requests, and what
the server must answer. The TypeScript server runs them in `server/contract.test.ts`; the Rust server replays
the same files (issue #94), so both are held to one description. A fixture says **what** is observable
(status, headers, body fields), never how the server gets there.

The id of a case is its path without `.json`, for example `auth/rejects-expired`.

## Format

```jsonc
{
  "title": "what this case shows",
  "config": { "oidc": true, "keyAlg": true },   // optional, both default to true
  "steps": [ { "token": ..., "request": { ... }, "expect": { ... } } ],
  "after": { "jwksFetches": 1 }                 // optional: checked once every step has run
}
```

`config.oidc: false` starts the server with no identity provider configured. `config.keyAlg: false` makes the
provider's key omit `alg`, as many providers do; then only the server's own algorithm allow-list stands between
a token with `alg: none` or `HS256` and a signature check.

### `token`

| Value | Meaning |
|---|---|
| `null` | no `Authorization` header |
| `"valid"` (the default when `token` is left out) | a good access token: `iss` the provider, `sub` `u1`, `aud` the resource, valid for 5 minutes |
| `{ "claims": {...}, "header": {...} }` | a good token with these claims and header fields replaced |
| `{ "signedBy": "attacker" }` | signed by a key the provider does not list |
| `{ "form": "opaque" }` | a string that is not a JWT |
| `{ "form": "tampered" }` | a valid header and signature around a payload nobody signed |

The harness mints tokens itself: it generates an RSA key pair, serves the public key from a fake provider, and
counts the key fetches. Nothing needs the network or a secret.

### `request`

`method` (default `POST`), `path` (default `/mcp`), `headers`, and `body`. A step that leaves them out sends a `POST` to `/mcp`. A JSON body is serialised;
`{ "$raw": "text" }` is sent as it is. `content-type` and `accept` are set to what MCP clients send unless given.

### `expect`

- `status`: the HTTP status.
- `headers`: name to exact value, `{ "contains": "text" }`, or `null` (the header must be absent). Names are lower case.
- `bodyText`: the exact body text (used for the empty 202).
- `body`: a list of checks on the parsed JSON body. A check names a `path` (a list of keys and indexes) and one of
  `equals` (deep equality), `contains` (a substring, or a member of an array), `startsWith`, `containsLine`,
  `gt`, `length`, or `exists`. `map: "name"` takes that property of each element of an array first.

### Placeholders

In any string: `$issuer`, `$resource` (`<app>/mcp`) and `$app` are replaced. A string that is exactly `$now`,
`$now+N` or `$now-N` becomes a number (seconds since the epoch). `{ "$repeat": "x", "times": N }` is a string of
N copies.

## Constants the cases rely on

- the body limit is 512 KiB (524288 bytes);
- the application origin is `https://dibibest.codecora.dev`, so the resource is `https://dibibest.codecora.dev/mcp`;
- the legacy versions are `2025-11-25`, `2025-06-18`, `2025-03-26`, the modern one is `2026-07-28`.

## Routes

`/mcp`; `GET /.well-known/oauth-protected-resource[/mcp]`; `GET /.well-known/oauth-authorization-server[/mcp]`;
`GET /.well-known/openid-configuration[/mcp]`. Any other path under those well-known prefixes must answer 404
(the Pages deployment has no function for it; the Rust server answers a JSON 404 and the fixture only checks the
status).

## Adding a case

Write the file, run `bun test server` and see it pass on the TypeScript server, then replay it on the Rust server.
If a case passes at once, break the implementation on purpose to make sure it can fail.

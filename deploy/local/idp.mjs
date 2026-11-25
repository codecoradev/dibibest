// A fake identity provider for the LOCAL rehearsal only (deploy/local). It is not Gatehouse and proves nothing about
// Gatehouse: it speaks just enough OpenID Connect (discovery, authorization code with PKCE, token, userinfo, keys)
// for dibibest's sign-in, and hands out ES256 access tokens for the MCP endpoint. Everything it knows is made up.
//
// It shares the network of the dibibest container (compose: network_mode "service:dibibest"), so the server reaches
// it at http://localhost:9000 and the browser (or the e2e script) reaches it on the port that container publishes.

import http from 'node:http';
import crypto from 'node:crypto';

const PORT = 9000;
// The issuer and the client id and secret come from the environment (compose sets them, next to the same values the
// server is given): nothing in this file is a credential, not even a made-up one.
const { IDP_ISSUER: ISSUER, IDP_CLIENT_ID: CLIENT_ID, IDP_CLIENT_SECRET: CLIENT_SECRET } = process.env;
if (!ISSUER || !CLIENT_ID || !CLIENT_SECRET) {
  console.error('IDP_ISSUER, IDP_CLIENT_ID and IDP_CLIENT_SECRET must be set');
  process.exit(2);
}
const KID = 'local-k1';

const { privateKey, publicKey } = crypto.generateKeyPairSync('ec', { namedCurve: 'P-256' });
const jwk = { ...publicKey.export({ format: 'jwk' }), kid: KID, alg: 'ES256', use: 'sig' };

const b64 = (b) => Buffer.from(b).toString('base64url');
function jwt(claims) {
  const head = b64(JSON.stringify({ alg: 'ES256', typ: 'at+jwt', kid: KID }));
  const body = b64(JSON.stringify(claims));
  const sig = crypto.sign('sha256', Buffer.from(`${head}.${body}`), { key: privateKey, dsaEncoding: 'ieee-p1363' });
  return `${head}.${body}.${b64(sig)}`;
}

const user = { sub: 'local-user-1', email: 'Rehearsal@Example.test', email_verified: true, name: 'Local Rehearsal' };
const codes = new Map(); // code -> { challenge, redirect_uri }
const tokens = new Map(); // opaque access token -> true

const doc = {
  issuer: ISSUER,
  authorization_endpoint: `${ISSUER}/oauth2/authorize`,
  token_endpoint: `${ISSUER}/oauth2/token`,
  userinfo_endpoint: `${ISSUER}/oauth2/userinfo`,
  jwks_uri: `${ISSUER}/jwks`,
  response_types_supported: ['code'],
  grant_types_supported: ['authorization_code'],
  code_challenge_methods_supported: ['S256'],
  token_endpoint_auth_methods_supported: ['client_secret_basic', 'client_secret_post'],
  scopes_supported: ['openid', 'profile', 'email'],
  id_token_signing_alg_values_supported: ['ES256'],
};

const json = (res, status, obj) => {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(obj));
};
const readBody = (req) => new Promise((ok) => { let s = ''; req.on('data', (c) => (s += c)); req.on('end', () => ok(s)); });

http.createServer(async (req, res) => {
  try {
    await handle(req, res);
  } catch (e) {
    console.log('bad request:', e.message);
    if (!res.headersSent) json(res, 400, { error: 'invalid_request' });
    else res.end();
  }
}).listen(PORT, '0.0.0.0', () => console.log(`fake identity provider on :${PORT}, issuer ${ISSUER}`));

async function handle(req, res) {
  const url = new URL(req.url, `http://localhost:${PORT}`);
  const path = url.pathname;
  console.log(req.method, path);

  if (path === '/api/auth/.well-known/openid-configuration' || path === '/api/auth/.well-known/oauth-authorization-server') {
    return json(res, 200, doc);
  }
  if (path === '/api/auth/jwks') return json(res, 200, { keys: [jwk] });

  if (path === '/api/auth/oauth2/authorize') {
    const q = url.searchParams;
    if (q.get('client_id') !== CLIENT_ID || q.get('response_type') !== 'code' || q.get('code_challenge_method') !== 'S256') {
      return json(res, 400, { error: 'invalid_request' });
    }
    const code = crypto.randomBytes(16).toString('hex');
    codes.set(code, { challenge: q.get('code_challenge'), redirect_uri: q.get('redirect_uri') });
    const back = new URL(q.get('redirect_uri'));
    back.searchParams.set('code', code);
    back.searchParams.set('state', q.get('state'));
    res.writeHead(302, { location: back.toString() }); // no consent screen: this is a rehearsal
    return res.end();
  }

  if (path === '/api/auth/oauth2/token' && req.method === 'POST') {
    const form = new URLSearchParams(await readBody(req));
    let id = form.get('client_id'), secret = form.get('client_secret');
    const basic = /^Basic (.+)$/.exec(req.headers.authorization ?? '');
    if (basic) {
      const [i, s] = Buffer.from(basic[1], 'base64').toString().split(':');
      id = decodeURIComponent(i); secret = decodeURIComponent(s);
    }
    if (id !== CLIENT_ID || secret !== CLIENT_SECRET) return json(res, 401, { error: 'invalid_client' });
    const entry = codes.get(form.get('code'));
    codes.delete(form.get('code')); // a code works once
    if (!entry || form.get('redirect_uri') !== entry.redirect_uri) return json(res, 400, { error: 'invalid_grant' });
    const challenge = crypto.createHash('sha256').update(form.get('code_verifier') ?? '').digest('base64url');
    if (challenge !== entry.challenge) return json(res, 400, { error: 'invalid_grant', error_description: 'PKCE' });
    const access = crypto.randomBytes(24).toString('hex');
    tokens.set(access, true);
    return json(res, 200, { access_token: access, token_type: 'Bearer', expires_in: 600 });
  }

  if (path === '/api/auth/oauth2/userinfo') {
    const m = /^Bearer (.+)$/.exec(req.headers.authorization ?? '');
    if (!m || !tokens.has(m[1])) return json(res, 401, { error: 'invalid_token' });
    return json(res, 200, user);
  }

  // Not part of OpenID: the rehearsal asks for an access token for the MCP endpoint directly.
  // GET /dev/token?aud=<resource>[&iss=<issuer>][&exp_in=<seconds>]
  if (path === '/dev/token') {
    const now = Math.floor(Date.now() / 1000);
    const claims = {
      iss: url.searchParams.get('iss') ?? ISSUER,
      sub: user.sub,
      aud: url.searchParams.get('aud') ?? 'http://localhost:8088/mcp',
      iat: now,
      exp: now + Number(url.searchParams.get('exp_in') ?? 600),
      scope: 'openid profile email',
    };
    res.writeHead(200, { 'content-type': 'text/plain' });
    return res.end(jwt(claims));
  }

  json(res, 404, { error: 'not_found' });
}

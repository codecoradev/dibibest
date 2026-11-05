// Route logic for the auth Pages Functions. Kept out of `functions/` (where every
// file is a route) so it can be unit-tested with a fake identity provider.
import {
	type Discovery,
	config,
	cookie,
	currentUser,
	discover,
	json,
	LOGIN_COOKIE,
	LOGIN_TTL_S,
	nowS,
	pkceChallenge,
	randomToken,
	readCookie,
	redirectUri,
	safeReturnTo,
	sameOrigin,
	SESSION_COOKIE,
	SESSION_TTL_S,
	sign,
	verify,
	type Env
} from './auth';

type Fetch = typeof fetch;

/** Discovery, or null when the identity provider cannot be reached right now. */
async function tryDiscover(issuer: string, fetcher: Fetch): Promise<Discovery | null> {
	try {
		return await discover(issuer, fetcher);
	} catch {
		return null;
	}
}

/** JSON body, or null for a non-JSON answer (e.g. a proxy error page with 200). */
async function readJson<T>(res: Response): Promise<T | null> {
	try {
		return (await res.json()) as T;
	} catch {
		return null;
	}
}

const UNREACHABLE = 'CodeCora ID cannot be reached right now. The workspace still works without signing in; try again in a moment.';

/** GET /auth/me → {enabled, user}. The web app reads this to gate import/export. */
export async function handleMe(req: Request, env: Env): Promise<Response> {
	const cfg = config(env);
	if (!cfg) return json({ enabled: false, user: null });
	return json({ enabled: true, user: await currentUser(req, cfg) });
}

/** GET /auth/login?return_to=/app → 302 to Gatehouse with PKCE S256 + state. */
export async function handleLogin(req: Request, env: Env, fetcher: Fetch = fetch): Promise<Response> {
	const cfg = config(env);
	if (!cfg) return json({ error: 'sign-in is not configured' }, 404);
	const d = await tryDiscover(cfg.issuer, fetcher);
	if (!d) return page(503, 'Sign-in unavailable', UNREACHABLE);
	const state = randomToken();
	const verifier = randomToken(48);
	const returnTo = safeReturnTo(new URL(req.url).searchParams.get('return_to'));
	const handshake = await sign(cfg.sessionSecret, { state, verifier, returnTo, exp: nowS() + LOGIN_TTL_S });
	const url = new URL(d.authorization_endpoint);
	url.search = new URLSearchParams({
		response_type: 'code',
		client_id: cfg.clientId,
		redirect_uri: redirectUri(req),
		scope: 'openid email profile',
		state,
		code_challenge: await pkceChallenge(verifier),
		code_challenge_method: 'S256'
	}).toString();
	return new Response(null, {
		status: 302,
		headers: {
			location: url.toString(),
			'set-cookie': cookie(req, LOGIN_COOKIE, handshake, LOGIN_TTL_S, '/auth'),
			'cache-control': 'no-store'
		}
	});
}

function page(status: number, title: string, body: string, setCookie?: string): Response {
	const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]!);
	const html = `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${esc(title)}</title><body style="font-family:system-ui,sans-serif;background:#0b0d10;color:#e6e8eb;display:grid;place-items:center;min-height:100vh;margin:0"><main style="max-width:28rem;padding:1.5rem"><h1 style="font-size:1.25rem">${esc(title)}</h1><p>${esc(body)}</p><p><a style="color:#6ea8ff" href="/app">Back to the workspace</a></p></main></body>`;
	const headers: Record<string, string> = { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' };
	if (setCookie) headers['set-cookie'] = setCookie;
	return new Response(html, { status, headers });
}

/** GET /auth/callback?code&state → code exchange, userinfo, session cookie. */
export async function handleCallback(req: Request, env: Env, fetcher: Fetch = fetch): Promise<Response> {
	const cfg = config(env);
	if (!cfg) return json({ error: 'sign-in is not configured' }, 404);
	const clearLogin = cookie(req, LOGIN_COOKIE, '', 0, '/auth');
	const params = new URL(req.url).searchParams;
	if (params.get('error')) {
		return page(400, 'Sign-in was cancelled', `The identity provider answered: ${params.get('error')}.`, clearLogin);
	}
	const raw = readCookie(req, LOGIN_COOKIE);
	const hs = raw
		? await verify<{ state: string; verifier: string; returnTo: string; exp: number }>(cfg.sessionSecret, raw)
		: null;
	const code = params.get('code');
	if (!hs || !code || params.get('state') !== hs.state) {
		return page(400, 'Sign-in expired', 'Please start signing in again.', clearLogin);
	}

	const d = await tryDiscover(cfg.issuer, fetcher);
	if (!d) return page(503, 'Sign-in unavailable', UNREACHABLE, clearLogin);
	const form = new URLSearchParams({
		grant_type: 'authorization_code',
		code,
		redirect_uri: redirectUri(req),
		code_verifier: hs.verifier
	});
	const headers: Record<string, string> = {
		'content-type': 'application/x-www-form-urlencoded',
		accept: 'application/json'
	};
	if (cfg.tokenAuth === 'basic') {
		// RFC 6749 §2.3.1: form-urlencode id and secret before Base64
		const id = encodeURIComponent(cfg.clientId);
		const secret = encodeURIComponent(cfg.clientSecret);
		headers.authorization = `Basic ${btoa(`${id}:${secret}`)}`;
	} else {
		form.set('client_id', cfg.clientId);
		form.set('client_secret', cfg.clientSecret);
	}
	const tok = await fetcher(d.token_endpoint, { method: 'POST', headers, body: form }).catch(() => null);
	if (!tok) return page(503, 'Sign-in unavailable', UNREACHABLE, clearLogin);
	if (!tok.ok) return page(502, 'Sign-in failed', `Token exchange failed (${tok.status}).`, clearLogin);
	const access_token = (await readJson<{ access_token?: string }>(tok))?.access_token;
	if (!access_token) return page(502, 'Sign-in failed', 'No access token was returned.', clearLogin);

	const ui = await fetcher(d.userinfo_endpoint, { headers: { authorization: `Bearer ${access_token}` } }).catch(
		() => null
	);
	if (!ui) return page(503, 'Sign-in unavailable', UNREACHABLE, clearLogin);
	if (!ui.ok) return page(502, 'Sign-in failed', `Could not read your profile (${ui.status}).`, clearLogin);
	const info =
		(await readJson<{ sub?: string; email?: string; email_verified?: boolean; name?: string }>(ui)) ?? {};
	if (!info.sub || !info.email) return page(502, 'Sign-in failed', 'Your profile has no email address.', clearLogin);
	if (info.email_verified === false) {
		return page(403, 'Email not verified', 'Verify your email with CodeCora ID, then sign in again.', clearLogin);
	}

	const session = await sign(cfg.sessionSecret, {
		sub: info.sub,
		email: info.email.toLowerCase(),
		name: info.name,
		exp: nowS() + SESSION_TTL_S
	});
	const res = new Response(null, { status: 302, headers: { location: hs.returnTo, 'cache-control': 'no-store' } });
	res.headers.append('set-cookie', cookie(req, SESSION_COOKIE, session, SESSION_TTL_S));
	res.headers.append('set-cookie', clearLogin);
	return res;
}

/** POST /auth/logout → clear the session (same-origin only). */
export async function handleLogout(req: Request): Promise<Response> {
	if (req.method !== 'POST') return json({ error: 'method not allowed' }, 405, { allow: 'POST' });
	if (!sameOrigin(req)) return json({ error: 'cross-site request refused' }, 403);
	return new Response(null, {
		status: 303,
		headers: { location: '/', 'set-cookie': cookie(req, SESSION_COOKIE, '', 0), 'cache-control': 'no-store' }
	});
}

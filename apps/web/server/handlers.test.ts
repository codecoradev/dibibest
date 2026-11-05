import { describe, expect, test } from 'bun:test';
import {
	config,
	pkceChallenge,
	safeReturnTo,
	sign,
	verify,
	LOGIN_COOKIE,
	SESSION_COOKIE,
	type Env
} from './auth';
import { handleCallback, handleLogin, handleLogout, handleMe } from './handlers';

const ISSUER = 'https://idp.test/api/auth';
const ENV: Env = {
	OIDC_ISSUER: ISSUER,
	OIDC_CLIENT_ID: 'dibibest-web',
	OIDC_CLIENT_SECRET: 's3cret:with/odd chars',
	SESSION_SECRET: 'x'.repeat(16) + 'session-secret-long-enough'
};
const APP = 'https://dibibest.pages.dev';

/** A fake Gatehouse: discovery, token, userinfo. Records what it received. */
function fakeIdp(userinfo: Record<string, unknown> = { sub: 'u1', email: 'Ana@Example.com', email_verified: true }) {
	const seen: { tokenBody?: URLSearchParams; tokenAuth?: string | null; bearer?: string | null } = {};
	const fetcher = (async (input: RequestInfo | URL, init?: RequestInit) => {
		const url = String(input);
		if (url === `${ISSUER}/.well-known/openid-configuration`) {
			return Response.json({
				authorization_endpoint: `${ISSUER}/oauth2/authorize`,
				token_endpoint: `${ISSUER}/oauth2/token`,
				userinfo_endpoint: `${ISSUER}/oauth2/userinfo`
			});
		}
		if (url === `${ISSUER}/oauth2/token`) {
			seen.tokenBody = new URLSearchParams(String(init?.body));
			seen.tokenAuth = new Headers(init?.headers).get('authorization');
			return Response.json({ access_token: 'at-1', token_type: 'Bearer' });
		}
		if (url === `${ISSUER}/oauth2/userinfo`) {
			seen.bearer = new Headers(init?.headers).get('authorization');
			return Response.json(userinfo);
		}
		return new Response('not found', { status: 404 });
	}) as typeof fetch;
	return { fetcher, seen };
}

const cookieValue = (setCookie: string | null, name: string) =>
	setCookie
		?.split(/,(?=\s*\w+=)/)
		.map((c) => c.trim())
		.find((c) => c.startsWith(`${name}=`))
		?.split(';')[0]
		.slice(name.length + 1) ?? null;

describe('config', () => {
	test('off unless every value is real', () => {
		expect(config(ENV)).not.toBeNull();
		expect(config({ ...ENV, OIDC_CLIENT_ID: '' })).toBeNull();
		expect(config({ ...ENV, OIDC_CLIENT_SECRET: 'changeme' })).toBeNull();
		expect(config({ ...ENV, SESSION_SECRET: 'too-short' })).toBeNull();
	});

	test('/auth/me reports disabled sign-in', async () => {
		const res = await handleMe(new Request(`${APP}/auth/me`), { OIDC_ISSUER: ISSUER });
		expect(await res.json()).toEqual({ enabled: false, user: null });
	});
});

describe('signed values', () => {
	const secret = ENV.SESSION_SECRET!;
	test('round-trip, tamper, expiry', async () => {
		const exp = Math.floor(Date.now() / 1000) + 60;
		const t = await sign(secret, { email: 'a@b.c', exp });
		expect((await verify<{ email: string; exp: number }>(secret, t))?.email).toBe('a@b.c');
		const [body, mac] = t.split('.');
		const forged = btoa(JSON.stringify({ email: 'evil@x', exp })).replace(/=+$/, '');
		expect(await verify(secret, `${forged}.${mac}`)).toBeNull();
		expect(await verify('another-secret-of-sufficient-length!!', t)).toBeNull();
		expect(await verify(secret, `${body}.${mac}`, exp + 1)).toBeNull();
		expect(await verify(secret, 'garbage')).toBeNull();
		expect(await verify(secret, `${body}.${mac}.extra`)).toBeNull();
	});
});

test('return_to only accepts same-site paths', () => {
	expect(safeReturnTo('/app')).toBe('/app');
	expect(safeReturnTo('//evil.test/x')).toBe('/app');
	expect(safeReturnTo('https://evil.test')).toBe('/app');
	expect(safeReturnTo('/\\evil.test')).toBe('/app');
	expect(safeReturnTo(null)).toBe('/app');
	// browsers strip tab/CR/LF and read `\\` as `/` in a Location: all must fall back
	for (const v of ['/\t/evil.test', '/\n/evil.test', '/\r/evil.test', '/\\/evil.test', '/x\\..\\..//evil.test', '/\u0000/evil']) {
		expect(safeReturnTo(v)).toBe('/app');
	}
	// what /auth/login receives for ?return_to=/%09/evil.test
	expect(safeReturnTo(new URLSearchParams('return_to=/%09/evil.test').get('return_to'))).toBe('/app');
	// ordinary paths survive, normalized
	expect(safeReturnTo('/app?x=1#y')).toBe('/app?x=1#y');
	expect(safeReturnTo('/a/../app')).toBe('/app');
});

describe('login → callback → me', () => {
	test('full flow with PKCE and client_secret_basic', async () => {
		const { fetcher, seen } = fakeIdp();
		const login = await handleLogin(new Request(`${APP}/auth/login?return_to=/app`), ENV, fetcher);
		expect(login.status).toBe(302);
		const authorize = new URL(login.headers.get('location')!);
		expect(authorize.origin + authorize.pathname).toBe(`${ISSUER}/oauth2/authorize`);
		expect(authorize.searchParams.get('client_id')).toBe('dibibest-web');
		expect(authorize.searchParams.get('redirect_uri')).toBe(`${APP}/auth/callback`);
		expect(authorize.searchParams.get('code_challenge_method')).toBe('S256');
		const setLogin = login.headers.get('set-cookie')!;
		expect(setLogin).toContain('HttpOnly');
		expect(setLogin).toContain('Secure');
		expect(setLogin).toContain('Path=/auth');
		const handshake = cookieValue(setLogin, LOGIN_COOKIE)!;
		const hs = await verify<{ state: string; verifier: string; exp: number }>(ENV.SESSION_SECRET!, handshake);
		expect(await pkceChallenge(hs!.verifier)).toBe(authorize.searchParams.get('code_challenge')!);

		const state = authorize.searchParams.get('state')!;
		const cb = await handleCallback(
			new Request(`${APP}/auth/callback?code=c-1&state=${state}`, { headers: { cookie: `${LOGIN_COOKIE}=${handshake}` } }),
			ENV,
			fetcher
		);
		expect(cb.status).toBe(302);
		expect(cb.headers.get('location')).toBe('/app');
		expect(seen.tokenBody?.get('code')).toBe('c-1');
		expect(seen.tokenBody?.get('code_verifier')).toBe(hs!.verifier);
		expect(seen.tokenBody?.get('client_secret')).toBeNull();
		const basic = atob(seen.tokenAuth!.replace('Basic ', ''));
		expect(basic).toBe(`dibibest-web:${encodeURIComponent(ENV.OIDC_CLIENT_SECRET!)}`);
		expect(seen.bearer).toBe('Bearer at-1');

		const session = cookieValue(cb.headers.get('set-cookie'), SESSION_COOKIE)!;
		const me = await handleMe(new Request(`${APP}/auth/me`, { headers: { cookie: `${SESSION_COOKIE}=${session}` } }), ENV);
		expect(await me.json()).toEqual({ enabled: true, user: { sub: 'u1', email: 'ana@example.com' } });
	});

	test('client_secret_post when configured', async () => {
		const { fetcher, seen } = fakeIdp();
		const env = { ...ENV, OIDC_TOKEN_AUTH: 'post' };
		const login = await handleLogin(new Request(`${APP}/auth/login`), env, fetcher);
		const handshake = cookieValue(login.headers.get('set-cookie'), LOGIN_COOKIE)!;
		const state = new URL(login.headers.get('location')!).searchParams.get('state')!;
		await handleCallback(
			new Request(`${APP}/auth/callback?code=c&state=${state}`, { headers: { cookie: `${LOGIN_COOKIE}=${handshake}` } }),
			env,
			fetcher
		);
		expect(seen.tokenAuth).toBeNull();
		expect(seen.tokenBody?.get('client_secret')).toBe(ENV.OIDC_CLIENT_SECRET!);
	});

	test('rejects a wrong state, a missing handshake and an unverified email', async () => {
		const { fetcher } = fakeIdp();
		const login = await handleLogin(new Request(`${APP}/auth/login`), ENV, fetcher);
		const handshake = cookieValue(login.headers.get('set-cookie'), LOGIN_COOKIE)!;
		const wrong = await handleCallback(
			new Request(`${APP}/auth/callback?code=c&state=nope`, { headers: { cookie: `${LOGIN_COOKIE}=${handshake}` } }),
			ENV,
			fetcher
		);
		expect(wrong.status).toBe(400);
		expect(wrong.headers.get('set-cookie')).not.toContain(SESSION_COOKIE);
		const missing = await handleCallback(new Request(`${APP}/auth/callback?code=c&state=x`), ENV, fetcher);
		expect(missing.status).toBe(400);

		const idp = fakeIdp({ sub: 'u2', email: 'x@y.z', email_verified: false });
		const l2 = await handleLogin(new Request(`${APP}/auth/login`), ENV, idp.fetcher);
		const h2 = cookieValue(l2.headers.get('set-cookie'), LOGIN_COOKIE)!;
		const s2 = new URL(l2.headers.get('location')!).searchParams.get('state')!;
		const unverified = await handleCallback(
			new Request(`${APP}/auth/callback?code=c&state=${s2}`, { headers: { cookie: `${LOGIN_COOKIE}=${h2}` } }),
			ENV,
			idp.fetcher
		);
		expect(unverified.status).toBe(403);
		expect(unverified.headers.get('set-cookie')).not.toContain(`${SESSION_COOKIE}=`);
	});

	test('a return_to pointing off-site is ignored', async () => {
		const { fetcher } = fakeIdp();
		const login = await handleLogin(new Request(`${APP}/auth/login?return_to=//evil.test`), ENV, fetcher);
		const hs = await verify<{ returnTo: string; exp: number }>(
			ENV.SESSION_SECRET!,
			cookieValue(login.headers.get('set-cookie'), LOGIN_COOKIE)!
		);
		expect(hs?.returnTo).toBe('/app');
	});
});

describe('logout', () => {
	test('POST only, same origin only, clears the session', async () => {
		expect((await handleLogout(new Request(`${APP}/auth/logout`))).status).toBe(405);
		const cross = await handleLogout(
			new Request(`${APP}/auth/logout`, { method: 'POST', headers: { origin: 'https://evil.test' } })
		);
		expect(cross.status).toBe(403);
		const ok = await handleLogout(new Request(`${APP}/auth/logout`, { method: 'POST', headers: { origin: APP } }));
		expect(ok.status).toBe(303);
		expect(ok.headers.get('set-cookie')).toContain(`${SESSION_COOKIE}=;`);
		expect(ok.headers.get('set-cookie')).toContain('Max-Age=0');
	});
});

test('an unreachable identity provider gives a clear 503, not a crash', async () => {
	const down = (async () => {
		throw new TypeError('network down');
	}) as unknown as typeof fetch;
	const env = { ...ENV, OIDC_ISSUER: 'https://down.test/api/auth' };
	const res = await handleLogin(new Request(`${APP}/auth/login`), env, down);
	expect(res.status).toBe(503);
	expect(await res.text()).toContain('cannot be reached');
});

test('a non-JSON answer from the token or userinfo endpoint fails cleanly', async () => {
	for (const broken of ['token', 'userinfo'] as const) {
		const { fetcher: good } = fakeIdp();
		const fetcher = (async (input: RequestInfo | URL, init?: RequestInit) => {
			if (String(input).endsWith(broken === 'token' ? '/oauth2/token' : '/oauth2/userinfo')) {
				return new Response('<html>proxy error</html>', { status: 200, headers: { 'content-type': 'text/html' } });
			}
			return good(input, init);
		}) as typeof fetch;
		const login = await handleLogin(new Request(`${APP}/auth/login`), ENV, fetcher);
		const handshake = cookieValue(login.headers.get('set-cookie'), LOGIN_COOKIE)!;
		const state = new URL(login.headers.get('location')!).searchParams.get('state')!;
		const res = await handleCallback(
			new Request(`${APP}/auth/callback?code=c&state=${state}`, { headers: { cookie: `${LOGIN_COOKIE}=${handshake}` } }),
			ENV,
			fetcher
		);
		expect(res.status).toBe(502);
		expect(res.headers.get('content-type')).toContain('text/html');
		expect(res.headers.get('set-cookie')).toContain(`${LOGIN_COOKIE}=;`);
		expect(res.headers.get('set-cookie')).not.toContain(`${SESSION_COOKIE}=`);
	}
});

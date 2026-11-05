// Stateless OIDC relying party for Cloudflare Pages Functions (epic #52, item 2).
// Gatehouse is the identity provider; nothing is stored server-side: the
// login handshake and the session are HMAC-signed cookies. Pure Web Crypto, so
// it runs in Workers and in `bun test`.

export type Env = {
	OIDC_ISSUER?: string;
	OIDC_CLIENT_ID?: string;
	OIDC_CLIENT_SECRET?: string;
	SESSION_SECRET?: string;
	/** `basic` (default, RFC 6749 §2.3.1) or `post`. */
	OIDC_TOKEN_AUTH?: string;
};

export type Config = {
	issuer: string;
	clientId: string;
	clientSecret: string;
	sessionSecret: string;
	tokenAuth: 'basic' | 'post';
};

export type User = { sub: string; email: string; name?: string };

export const SESSION_COOKIE = 'dibibest_session';
export const LOGIN_COOKIE = 'dibibest_login';
export const SESSION_TTL_S = 7 * 24 * 3600;
export const LOGIN_TTL_S = 10 * 60;

const PLACEHOLDER = /^(\.\.\.|x+|changeme|todo)$/i;

/** Auth is ON only when every required value is real (empty/placeholder = off). */
export function config(env: Env): Config | null {
	const real = (v?: string) => (v && v.trim() && !PLACEHOLDER.test(v.trim()) ? v.trim() : null);
	const issuer = real(env.OIDC_ISSUER);
	const clientId = real(env.OIDC_CLIENT_ID);
	const clientSecret = real(env.OIDC_CLIENT_SECRET);
	const sessionSecret = real(env.SESSION_SECRET);
	if (!issuer || !clientId || !clientSecret || !sessionSecret || sessionSecret.length < 32) return null;
	return {
		issuer: issuer.replace(/\/+$/, ''),
		clientId,
		clientSecret,
		sessionSecret,
		tokenAuth: env.OIDC_TOKEN_AUTH === 'post' ? 'post' : 'basic'
	};
}

// ── encoding ────────────────────────────────────────────────────────────────

const enc = new TextEncoder();

export function b64url(bytes: Uint8Array): string {
	let s = '';
	for (const b of bytes) s += String.fromCharCode(b);
	return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function b64urlDecode(s: string): Uint8Array<ArrayBuffer> | null {
	if (!/^[A-Za-z0-9_-]*$/.test(s)) return null;
	try {
		const bin = atob(s.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (s.length % 4)) % 4));
		const out = new Uint8Array(new ArrayBuffer(bin.length));
		for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
		return out;
	} catch {
		return null;
	}
}

export function randomToken(bytes = 32): string {
	return b64url(crypto.getRandomValues(new Uint8Array(bytes)));
}

/** PKCE S256 challenge for `verifier` (RFC 7636). */
export async function pkceChallenge(verifier: string): Promise<string> {
	return b64url(new Uint8Array(await crypto.subtle.digest('SHA-256', enc.encode(verifier))));
}

// ── signed values ───────────────────────────────────────────────────────────

async function hmacKey(secret: string): Promise<CryptoKey> {
	return crypto.subtle.importKey('raw', enc.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, [
		'sign',
		'verify'
	]);
}

/** `b64url(json).b64url(HMAC-SHA256)`; `payload.exp` (unix seconds) is required. */
export async function sign(secret: string, payload: { exp: number } & Record<string, unknown>): Promise<string> {
	const body = b64url(enc.encode(JSON.stringify(payload)));
	const mac = new Uint8Array(await crypto.subtle.sign('HMAC', await hmacKey(secret), enc.encode(body)));
	return `${body}.${b64url(mac)}`;
}

/** The payload when the signature is valid (constant-time) and `exp` is in the future. */
export async function verify<T extends { exp: number }>(secret: string, token: string, now = nowS()): Promise<T | null> {
	const [body, mac, extra] = token.split('.');
	if (!body || !mac || extra !== undefined) return null;
	const sig = b64urlDecode(mac);
	if (!sig) return null;
	// crypto.subtle.verify compares in constant time
	const ok = await crypto.subtle.verify('HMAC', await hmacKey(secret), sig, enc.encode(body));
	if (!ok) return null;
	const raw = b64urlDecode(body);
	if (!raw) return null;
	try {
		const v = JSON.parse(new TextDecoder().decode(raw));
		return typeof v?.exp === 'number' && v.exp > now ? (v as T) : null;
	} catch {
		return null;
	}
}

export const nowS = () => Math.floor(Date.now() / 1000);

// ── cookies ─────────────────────────────────────────────────────────────────

export function readCookie(req: Request, name: string): string | null {
	for (const part of (req.headers.get('cookie') ?? '').split(';')) {
		const [k, ...v] = part.trim().split('=');
		if (k === name) return v.join('=');
	}
	return null;
}

export function cookie(req: Request, name: string, value: string, maxAge: number, path = '/'): string {
	const secure = new URL(req.url).protocol === 'https:' ? '; Secure' : '';
	return `${name}=${value}; Path=${path}; Max-Age=${maxAge}; HttpOnly; SameSite=Lax${secure}`;
}

// ── flow helpers ────────────────────────────────────────────────────────────

/**
 * Only same-site relative paths (open-redirect guard). Rejects control
 * characters and backslashes outright, because browsers strip tabs/newlines and
 * treat `\\` as `/` when parsing a Location (`/\t/evil` → `//evil`), then
 * resolves the path against a placeholder origin and requires it to stay there.
 */
export function safeReturnTo(v: string | null): string {
	const fallback = '/app';
	// eslint-disable-next-line no-control-regex
	if (!v || /[\u0000-\u001f\u007f\\]/.test(v)) return fallback;
	if (!v.startsWith('/') || v.startsWith('//')) return fallback;
	const base = 'https://return-to.invalid';
	try {
		const u = new URL(v, base);
		if (u.origin !== base) return fallback;
		return `${u.pathname}${u.search}${u.hash}`;
	} catch {
		return fallback;
	}
}

/** CSRF guard for state-changing requests: same origin, or same-site fetch metadata. */
export function sameOrigin(req: Request): boolean {
	const origin = req.headers.get('origin');
	if (origin) return origin === new URL(req.url).origin;
	const site = req.headers.get('sec-fetch-site');
	return site === 'same-origin' || site === 'same-site';
}

export function redirectUri(req: Request): string {
	return new URL('/auth/callback', req.url).toString();
}

export type Discovery = { authorization_endpoint: string; token_endpoint: string; userinfo_endpoint: string };

let discoveryCache: { issuer: string; doc: Discovery; at: number } | undefined;

export async function discover(issuer: string, fetcher: typeof fetch = fetch): Promise<Discovery> {
	if (discoveryCache?.issuer === issuer && Date.now() - discoveryCache.at < 3600_000) return discoveryCache.doc;
	const res = await fetcher(`${issuer}/.well-known/openid-configuration`, { headers: { accept: 'application/json' } });
	if (!res.ok) throw new Error(`discovery failed: ${res.status}`);
	const doc = (await res.json()) as Discovery;
	if (!doc.authorization_endpoint || !doc.token_endpoint || !doc.userinfo_endpoint) {
		throw new Error('discovery document is missing endpoints');
	}
	discoveryCache = { issuer, doc, at: Date.now() };
	return doc;
}

export function json(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json', 'cache-control': 'no-store', ...headers }
	});
}

/** The signed-in user from the session cookie, or null. */
export async function currentUser(req: Request, cfg: Config): Promise<User | null> {
	const raw = readCookie(req, SESSION_COOKIE);
	if (!raw) return null;
	const s = await verify<User & { exp: number }>(cfg.sessionSecret, raw);
	return s ? { sub: s.sub, email: s.email, name: s.name } : null;
}

// Contract tests for the remote MCP: every case is a JSON file in fixtures/mcp (format: fixtures/mcp/README.md).
// The Rust server replays the same files (#94), so both implementations are held to one description of behaviour.
import { beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { b64url, type Env } from './auth';
import { makeCore, type Core } from './core';
import { resetJwksCache } from './jwt';
import { handleAuthorizationServerMetadata, handleMcp, handleProtectedResource } from './mcp';

const ISSUER = 'https://idp-mcp.test/api/auth';
const APP = 'https://dibibest.codecora.dev';
const RESOURCE = `${APP}/mcp`;
const KID = 'k1';
const FIXTURES = join(import.meta.dir, 'fixtures', 'mcp');

type Json = null | boolean | number | string | Json[] | { [k: string]: Json };
type HeaderWant = string | null | { contains: string };
type BodyCheck = {
	path: (string | number)[];
	map?: string;
	equals?: Json;
	contains?: Json;
	startsWith?: string;
	containsLine?: string;
	gt?: number;
	length?: number;
	exists?: boolean;
};
type Step = {
	token?: 'valid' | null | { claims?: Record<string, Json>; header?: Record<string, Json>; signedBy?: 'idp' | 'attacker'; form?: 'jwt' | 'opaque' | 'tampered' };
	request: { method?: string; path?: string; headers?: Record<string, string>; body?: Json | { $raw: string } };
	expect: { status: number; headers?: Record<string, HeaderWant>; bodyText?: string; body?: BodyCheck[] };
};
type Fixture = { id: string; title: string; config?: { oidc?: boolean; keyAlg?: boolean }; steps: Step[]; after?: { jwksFetches?: number } };

function load(dir: string): Fixture[] {
	const out: Fixture[] = [];
	for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
		const p = join(dir, entry.name);
		if (entry.isDirectory()) out.push(...load(p));
		else if (entry.name.endsWith('.json')) {
			const f = JSON.parse(readFileSync(p, 'utf8')) as Omit<Fixture, 'id'>;
			out.push({ ...f, id: relative(FIXTURES, p).replace(/\.json$/, '') });
		}
	}
	return out;
}

const RSA = { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]) };
let keys: CryptoKeyPair;
let attacker: CryptoKeyPair;
let publicJwk: JsonWebKey;
let core: Core;
let jwksHits = 0;
/** Whether the provider's key lists `alg`. Many do not, and then only the server's own allow-list stops `alg: none`. */
let keyHasAlg = true;

beforeAll(async () => {
	keys = (await crypto.subtle.generateKey(RSA, true, ['sign', 'verify'])) as CryptoKeyPair;
	attacker = (await crypto.subtle.generateKey(RSA, true, ['sign', 'verify'])) as CryptoKeyPair;
	publicJwk = await crypto.subtle.exportKey('jwk', keys.publicKey);
	core = makeCore(new WebAssembly.Module(readFileSync(new URL('../src/lib/wasm/dibibest.wasm', import.meta.url))));
});

beforeEach(() => {
	jwksHits = 0;
	keyHasAlg = true;
	resetJwksCache();
});

/** A fake identity provider: discovery document and keys. Counts the key fetches. */
const idp = (async (input: RequestInfo | URL) => {
	const url = String(input);
	if (url === `${ISSUER}/.well-known/openid-configuration` || url === `${ISSUER}/.well-known/oauth-authorization-server`) {
		return Response.json({
			issuer: ISSUER,
			authorization_endpoint: `${ISSUER}/oauth2/authorize`,
			token_endpoint: `${ISSUER}/oauth2/token`,
			userinfo_endpoint: `${ISSUER}/oauth2/userinfo`,
			jwks_uri: `${ISSUER}/jwks`
		});
	}
	if (url === `${ISSUER}/jwks`) {
		jwksHits++;
		// exportKey already puts alg on the key, so take it off before deciding whether to publish it
		const { alg: _exported, ...material } = publicJwk;
		return Response.json({ keys: [{ ...material, kid: KID, ...(keyHasAlg ? { alg: 'RS256' } : {}), use: 'sig' }] });
	}
	return new Response('not found', { status: 404 });
}) as typeof fetch;

const now = () => Math.floor(Date.now() / 1000);
const enc = (v: unknown) => b64url(new TextEncoder().encode(JSON.stringify(v)));

/** Placeholders: $issuer, $resource, $app inside strings; a whole string `$now`, `$now+N`, `$now-N` is a number;
 * {"$repeat": "x", "times": N} is a string of N copies. */
function resolve(v: Json | undefined): Json {
	if (typeof v === 'string') {
		const m = /^\$now([+-]\d+)?$/.exec(v);
		if (m) return now() + (m[1] ? Number(m[1]) : 0);
		return v.replaceAll('$issuer', ISSUER).replaceAll('$resource', RESOURCE).replaceAll('$app', APP);
	}
	if (Array.isArray(v)) return v.map(resolve);
	if (v && typeof v === 'object') {
		if ('$repeat' in v) return String(v.$repeat).repeat(Number(v.times));
		return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, resolve(x)]));
	}
	return v ?? null;
}

async function mint(spec: Exclude<Step['token'], null | undefined>): Promise<string> {
	const s = spec === 'valid' ? {} : spec;
	if (s.form === 'opaque') return 'opaque-token-value';
	const header = enc({ alg: 'RS256', typ: 'at+jwt', kid: KID, ...(resolve(s.header ?? {}) as object) });
	const claims = { iss: ISSUER, sub: 'u1', aud: [RESOURCE, `${ISSUER}/oauth2/userinfo`], exp: now() + 300, iat: now(), ...(resolve(s.claims ?? {}) as object) };
	const key = s.signedBy === 'attacker' ? attacker.privateKey : keys.privateKey;
	const sig = b64url(new Uint8Array(await crypto.subtle.sign('RSASSA-PKCS1-v1_5', key, new TextEncoder().encode(`${header}.${enc(claims)}`))));
	if (s.form === 'tampered') {
		// a valid header and signature around a payload the signer never saw
		return `${header}.${enc({ iss: ISSUER, sub: 'admin', aud: RESOURCE, exp: now() + 300 })}.${sig}`;
	}
	return `${header}.${enc(claims)}.${sig}`;
}

/** The routes the deployment answers: /mcp and the well-known documents (anything else under a well-known
 * prefix reaches the same handler, which answers 404; the Rust server answers a JSON 404 for it). */
function route(req: Request, env: Env): Response | Promise<Response> {
	const path = new URL(req.url).pathname;
	if (path === '/mcp') return handleMcp(req, env, core, idp);
	if (req.method === 'GET' && /^\/\.well-known\/oauth-protected-resource(\/|$)/.test(path)) return handleProtectedResource(req, env);
	if (req.method === 'GET' && /^\/\.well-known\/(oauth-authorization-server|openid-configuration)(\/|$)/.test(path)) {
		return handleAuthorizationServerMetadata(req, env, idp);
	}
	return new Response('not found', { status: 404 });
}

function pick(root: unknown, path: (string | number)[]): unknown {
	return path.reduce<unknown>((v, k) => (v === null || v === undefined ? undefined : (v as Record<string | number, unknown>)[k]), root);
}

async function verifyResponse(res: Response, want: Step['expect']) {
	expect(res.status).toBe(want.status);
	for (const [name, w] of Object.entries(want.headers ?? {})) {
		const got = res.headers.get(name);
		if (w === null) expect(got, `header ${name} must be absent`).toBeNull();
		else if (typeof w === 'string') expect(got, `header ${name}`).toBe(resolve(w) as string);
		else expect(got ?? '', `header ${name}`).toContain(resolve(w.contains) as string);
	}
	const text = await res.text();
	if (want.bodyText !== undefined) expect(text).toBe(want.bodyText);
	if (!want.body) return;
	const body = JSON.parse(text) as unknown;
	for (const c of want.body) {
		let value = pick(body, c.path);
		const where = `body ${c.path.join('.')}`;
		if (c.exists !== undefined) {
			expect(value !== undefined, where).toBe(c.exists);
			continue;
		}
		if (c.map) value = (value as Record<string, unknown>[]).map((x) => x[c.map!]);
		if (c.equals !== undefined) expect(value, where).toEqual(resolve(c.equals));
		if (c.contains !== undefined) {
			if (typeof value === 'string') expect(value, where).toContain(resolve(c.contains) as string);
			else expect(value as unknown[], where).toContain(resolve(c.contains));
		}
		if (c.startsWith !== undefined) expect((value as string).startsWith(c.startsWith), where).toBe(true);
		if (c.containsLine !== undefined) expect((value as string).split('\n'), where).toContain(c.containsLine);
		if (c.gt !== undefined) expect(value as number, where).toBeGreaterThan(c.gt);
		if (c.length !== undefined) expect((value as unknown[]).length, where).toBe(c.length);
	}
}

const fixtures = load(FIXTURES);

test('the fixtures were found and are well formed', () => {
	// Every group must be present, so a runner that silently loads nothing (or loses a directory) fails here
	// instead of passing with no cases. No count is hard-coded: adding a fixture needs no change in this file.
	const groups = new Set(fixtures.map((f) => f.id.split('/')[0]));
	for (const g of ['metadata', 'auth', 'cors', 'legacy']) {
		expect(groups.has(g), `no fixtures found in the group ${g}`).toBe(true);
	}
	for (const f of fixtures) {
		expect(f.title, f.id).toBeTruthy();
		expect(f.steps.length, f.id).toBeGreaterThan(0);
		for (const s of f.steps) expect(s.expect.status, f.id).toBeGreaterThan(0);
	}
	expect(new Set(fixtures.map((f) => f.id)).size).toBe(fixtures.length);
});

for (const [group, items] of Object.entries(Object.groupBy(fixtures, (f) => f.id.split('/')[0]))) {
	describe(`mcp contract: ${group}`, () => {
		for (const f of items ?? []) {
			test(`${f.id}: ${f.title}`, async () => {
				const env: Env = f.config?.oidc === false ? {} : { OIDC_ISSUER: ISSUER };
				keyHasAlg = f.config?.keyAlg !== false;
				for (const step of f.steps) {
					const r = step.request;
					const headers: Record<string, string> = {
						'content-type': 'application/json',
						accept: 'application/json, text/event-stream',
						...(resolve((r.headers ?? {}) as Json) as Record<string, string>)
					};
					const token = step.token === undefined ? 'valid' : step.token; // a missing token is a valid one; null is none
					if (token) headers.authorization = `Bearer ${await mint(token)}`;
					const method = r.method ?? 'POST';
					let body: string | undefined;
					if (r.body !== undefined && method !== 'GET' && method !== 'OPTIONS') {
						body = typeof r.body === 'object' && r.body !== null && '$raw' in r.body ? String(r.body.$raw) : JSON.stringify(resolve(r.body as Json));
					}
					const res = await route(new Request(`${APP}${r.path ?? '/mcp'}`, { method, headers, body }), env);
					await verifyResponse(res, step.expect);
				}
				if (f.after?.jwksFetches !== undefined) expect(jwksHits).toBe(f.after.jwksFetches);
			});
		}
	});
}

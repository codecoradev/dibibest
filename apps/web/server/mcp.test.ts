import { afterEach, beforeAll, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { b64url, type Env } from './auth';
import { makeCore, type Core } from './core';
import { resetJwksCache, verifyAccessToken, TokenError } from './jwt';
import { handleAuthorizationServerMetadata, handleMcp, handleProtectedResource, MAX_BODY_BYTES } from './mcp';

const ISSUER = 'https://idp-mcp.test/api/auth';
const ENV: Env = { OIDC_ISSUER: ISSUER };
const APP = 'https://dibibest.codecora.dev';
const RESOURCE = `${APP}/mcp`;
const KID = 'k1';

let keys: CryptoKeyPair;
let attacker: CryptoKeyPair;
let publicJwk: JsonWebKey;
let core: Core;
let jwksHits = 0;

const RSA = { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]) };

beforeAll(async () => {
	keys = (await crypto.subtle.generateKey(RSA, true, ['sign', 'verify'])) as CryptoKeyPair;
	attacker = (await crypto.subtle.generateKey(RSA, true, ['sign', 'verify'])) as CryptoKeyPair;
	publicJwk = await crypto.subtle.exportKey('jwk', keys.publicKey);
	const wasm = new URL('../src/lib/wasm/dibibest.wasm', import.meta.url);
	core = makeCore(new WebAssembly.Module(readFileSync(wasm)));
});

afterEach(() => resetJwksCache());

const fetcher = (async (input: RequestInfo | URL) => {
	const url = String(input);
	if (url === `${ISSUER}/.well-known/openid-configuration`) {
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
		return Response.json({ keys: [{ ...publicJwk, kid: KID, alg: 'RS256', use: 'sig' }] });
	}
	return new Response('not found', { status: 404 });
}) as typeof fetch;

const now = () => Math.floor(Date.now() / 1000);
const enc = (v: unknown) => b64url(new TextEncoder().encode(JSON.stringify(v)));

async function jwt(
	claims: Record<string, unknown> = {},
	opts: { key?: CryptoKey; header?: Record<string, unknown> } = {}
): Promise<string> {
	const header = enc({ alg: 'RS256', typ: 'at+jwt', kid: KID, ...opts.header });
	const body = enc({ iss: ISSUER, sub: 'u1', aud: [RESOURCE, `${ISSUER}/oauth2/userinfo`], exp: now() + 300, iat: now(), ...claims });
	const sig = await crypto.subtle.sign('RSASSA-PKCS1-v1_5', opts.key ?? keys.privateKey, new TextEncoder().encode(`${header}.${body}`));
	return `${header}.${body}.${b64url(new Uint8Array(sig))}`;
}

function post(body: unknown, headers: Record<string, string> = {}) {
	return new Request(RESOURCE, {
		method: 'POST',
		headers: { 'content-type': 'application/json', accept: 'application/json, text/event-stream', ...headers },
		body: typeof body === 'string' ? body : JSON.stringify(body)
	});
}

async function rpc(method: string, params: unknown, token?: string) {
	const res = await handleMcp(
		post({ jsonrpc: '2.0', id: 1, method, params }, { authorization: `Bearer ${token ?? (await jwt())}` }),
		ENV,
		core,
		fetcher
	);
	return { res, body: (await res.json()) as any };
}

describe('protected resource metadata', () => {
	test('points at Gatehouse for this origin', async () => {
		const res = handleProtectedResource(new Request(`${APP}/.well-known/oauth-protected-resource/mcp`), ENV);
		expect(res.status).toBe(200);
		expect(res.headers.get('access-control-allow-origin')).toBe('*');
		const doc = (await res.json()) as any;
		expect(doc.resource).toBe(RESOURCE);
		expect(doc.authorization_servers).toEqual([ISSUER]);
	});

	test('404 without an issuer', () => {
		expect(handleProtectedResource(new Request(`${APP}/.well-known/oauth-protected-resource`), {}).status).toBe(404);
	});
});

describe('authorization server metadata on the MCP origin', () => {
	test('serves the issuer document unchanged at the RFC 8414 and OIDC paths', async () => {
		for (const p of ['/.well-known/oauth-authorization-server', '/.well-known/oauth-authorization-server/mcp', '/.well-known/openid-configuration']) {
			const res = await handleAuthorizationServerMetadata(new Request(`${APP}${p}`), ENV, fetcher);
			expect(res.status).toBe(200);
			expect(res.headers.get('content-type')).toContain('application/json');
			const doc = (await res.json()) as any;
			expect(doc.issuer).toBe(ISSUER);
			expect(doc.authorization_endpoint).toBe(`${ISSUER}/oauth2/authorize`);
		}
		const other = await handleAuthorizationServerMetadata(new Request(`${APP}/.well-known/oauth-authorization-server/other`), ENV, fetcher);
		expect(other.status).toBe(404);
	});
});

describe('auth', () => {
	test('no token → 401 pointing at the metadata', async () => {
		const res = await handleMcp(post({ jsonrpc: '2.0', id: 1, method: 'initialize' }), ENV, core, fetcher);
		expect(res.status).toBe(401);
		expect(res.headers.get('www-authenticate')).toBe(
			`Bearer resource_metadata="${APP}/.well-known/oauth-protected-resource/mcp"`
		);
	});

	test.each([
		['wrong audience', { aud: 'https://other.example/mcp' }, {}],
		['wrong issuer', { iss: 'https://evil.example' }, {}],
		['expired', { exp: now() - 3600 }, {}],
		['not yet valid', { nbf: now() + 3600 }, {}],
		['DPoP-bound', { cnf: { jkt: 'abc' } }, {}],
		['no subject', { sub: '' }, {}],
		['unknown kid', {}, { header: { kid: 'nope' } }],
		['alg none', {}, { header: { alg: 'none' } }],
		['HS256', {}, { header: { alg: 'HS256' } }]
	] as const)('%s → 401 invalid_token', async (_name, claims, opts) => {
		const { res } = await rpc('ping', {}, await jwt(claims, opts as never));
		expect(res.status).toBe(401);
		expect(res.headers.get('www-authenticate')).toContain('error="invalid_token"');
	});

	test('signature from another key → 401', async () => {
		const { res } = await rpc('ping', {}, await jwt({}, { key: attacker.privateKey }));
		expect(res.status).toBe(401);
	});

	test('tampered payload → 401', async () => {
		const [h, , s] = (await jwt()).split('.');
		const forged = `${h}.${enc({ iss: ISSUER, sub: 'admin', aud: RESOURCE, exp: now() + 300 })}.${s}`;
		const { res } = await rpc('ping', {}, forged);
		expect(res.status).toBe(401);
	});

	test('opaque token → 401', async () => {
		const { res } = await rpc('ping', {}, 'opaque-token-value');
		expect(res.status).toBe(401);
	});

	test('JWKS is cached between requests', async () => {
		jwksHits = 0;
		await rpc('ping', {});
		await rpc('ping', {});
		expect(jwksHits).toBe(1);
	});

	test('a single string aud is accepted', async () => {
		const { res } = await rpc('ping', {}, await jwt({ aud: RESOURCE }));
		expect(res.status).toBe(200);
	});

	test('direct verify returns the claims', async () => {
		const c = await verifyAccessToken(await jwt(), { issuer: ISSUER, audience: RESOURCE, jwksUri: `${ISSUER}/jwks`, fetcher });
		expect(c.sub).toBe('u1');
		await expect(
			verifyAccessToken('a.b', { issuer: ISSUER, audience: RESOURCE, jwksUri: `${ISSUER}/jwks`, fetcher })
		).rejects.toBeInstanceOf(TokenError);
	});
});

describe('transport', () => {
	test('not configured → 404', async () => {
		const res = await handleMcp(post({}), {}, core, fetcher);
		expect(res.status).toBe(404);
	});

	test('GET → 405 (no SSE stream)', async () => {
		const res = await handleMcp(new Request(RESOURCE), ENV, core, fetcher);
		expect(res.status).toBe(405);
	});

	test('browser origins (claude.ai) get CORS, not a 403; the token still decides', async () => {
		const origin = { origin: 'https://claude.ai' };
		// no token: 401, readable cross-origin (WWW-Authenticate exposed)
		const anon = await handleMcp(post({ jsonrpc: '2.0', id: 1, method: 'initialize' }, origin), ENV, core, fetcher);
		expect(anon.status).toBe(401);
		expect(anon.headers.get('access-control-allow-origin')).toBe('https://claude.ai');
		expect(anon.headers.get('access-control-expose-headers')).toContain('www-authenticate');
		expect(anon.headers.get('www-authenticate')).toContain('resource_metadata=');
		// with a token: served
		const ok = await handleMcp(post({ jsonrpc: '2.0', id: 1, method: 'tools/list' }, { ...origin, authorization: `Bearer ${await jwt()}` }), ENV, core, fetcher);
		expect(ok.status).toBe(200);
		expect(ok.headers.get('access-control-allow-origin')).toBe('https://claude.ai');
		// preflight
		const pre = await handleMcp(
			new Request(RESOURCE, { method: 'OPTIONS', headers: { ...origin, 'access-control-request-method': 'POST' } }),
			ENV,
			core,
			fetcher
		);
		expect(pre.status).toBe(204);
		expect(pre.headers.get('access-control-allow-headers')).toContain('authorization');
		expect(pre.headers.get('access-control-allow-headers')).toContain('mcp-protocol-version');
		// same-origin / server-side callers get no CORS headers
		const plain = await handleMcp(post({ jsonrpc: '2.0', id: 1, method: 'ping' }), ENV, core, fetcher);
		expect(plain.headers.get('access-control-allow-origin')).toBeNull();
	});

	test('notification → 202 without a body', async () => {
		const res = await handleMcp(
			post({ jsonrpc: '2.0', method: 'notifications/initialized' }, { authorization: `Bearer ${await jwt()}` }),
			ENV,
			core,
			fetcher
		);
		expect(res.status).toBe(202);
		expect(await res.text()).toBe('');
	});

	test('bad JSON, batches and oversized bodies are refused', async () => {
		const auth = { authorization: `Bearer ${await jwt()}` };
		expect((await handleMcp(post('{nope', auth), ENV, core, fetcher)).status).toBe(400);
		expect((await handleMcp(post([{ jsonrpc: '2.0', id: 1, method: 'ping' }], auth), ENV, core, fetcher)).status).toBe(400);
		const big = { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'convert_schema', arguments: { source: 'x'.repeat(MAX_BODY_BYTES), to: 'sql' } } };
		expect((await handleMcp(post(big, auth), ENV, core, fetcher)).status).toBe(413);
	});

	test('dual-era: modern (2026-07-28) requests are served statelessly, like uteke-cloud', async () => {
		const bearer = `Bearer ${await jwt()}`;
		const modern = (method: string, params: Record<string, unknown> = {}, extra: Record<string, string> = {}) =>
			handleMcp(
				post(
					{ jsonrpc: '2.0', id: 1, method, params: { ...params, _meta: { 'io.modelcontextprotocol/protocolVersion': '2026-07-28' } } },
					{ authorization: bearer, 'mcp-protocol-version': '2026-07-28', 'mcp-method': method, ...extra }
				),
				ENV,
				core,
				fetcher
			);
		// Claude opens with server/discover
		const discover = await handleMcp(
			post({ jsonrpc: '2.0', id: 1, method: 'server/discover' }, { authorization: bearer, 'mcp-protocol-version': '2026-07-28', 'mcp-method': 'server/discover' }),
			ENV,
			core,
			fetcher
		);
		expect(discover.status).toBe(200);
		const d = ((await discover.json()) as any).result;
		expect(d.supportedVersions[0]).toBe('2026-07-28');
		expect(d.supportedVersions).toContain('2025-06-18');
		expect(d.capabilities).toEqual({ tools: {} });
		expect(d.resultType).toBe('complete');
		expect(d._meta['io.modelcontextprotocol/serverInfo'].name).toBe('dibibest');

		const list = ((await (await modern('tools/list')).json()) as any).result;
		expect(list.tools.map((t: { name: string }) => t.name)).toEqual(['convert_schema', 'render_diagram', 'list_formats']);
		expect(list.ttlMs).toBeGreaterThan(0);
		expect(list.cacheScope).toBe('private');
		expect(list.resultType).toBe('complete');
		expect(list.tools[0].annotations.readOnlyHint).toBe(true);

		const call = await modern('tools/call', { name: 'list_formats', arguments: {} }, { 'mcp-name': 'list_formats' });
		const c = ((await call.json()) as any).result;
		expect(c.isError).toBe(false);
		expect(c.content[0].text).toContain('sql:mysql');
		expect(c.resultType).toBe('complete');
		// Mcp-Name may be base64-encoded
		const b64 = await modern('tools/call', { name: 'list_formats', arguments: {} }, { 'mcp-name': `=?base64?${btoa('list_formats')}?=` });
		expect(b64.status).toBe(200);

		// header validation, unsupported versions, removed methods
		const missing = await modern('tools/call', { name: 'list_formats', arguments: {} });
		expect(missing.status).toBe(400);
		expect(((await missing.json()) as any).error.code).toBe(-32020);
		const wrong = await modern('tools/list', {}, { 'mcp-method': 'tools/call' });
		expect(((await wrong.json()) as any).error.code).toBe(-32020);
		const future = await handleMcp(
			post(
				{ jsonrpc: '2.0', id: 1, method: 'tools/list', params: { _meta: { 'io.modelcontextprotocol/protocolVersion': '2099-01-01' } } },
				{ authorization: bearer, 'mcp-protocol-version': '2099-01-01', 'mcp-method': 'tools/list' }
			),
			ENV,
			core,
			fetcher
		);
		expect(future.status).toBe(400);
		const fe = ((await future.json()) as any).error;
		expect(fe.code).toBe(-32022);
		expect(fe.data.supported[0]).toBe('2026-07-28');
		const ping = await modern('ping');
		expect(ping.status).toBe(404);
		expect(((await ping.json()) as any).error.code).toBe(-32601);

		// the legacy handshake keeps working
		const init = await handleMcp(
			post({ jsonrpc: '2.0', id: 2, method: 'initialize', params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'claude-ai', version: '0.1' } } }, { authorization: bearer }),
			ENV,
			core,
			fetcher
		);
		expect(((await init.json()) as any).result.protocolVersion).toBe('2025-06-18');
		const legacyList = await handleMcp(post({ jsonrpc: '2.0', id: 3, method: 'tools/list' }, { authorization: bearer, 'mcp-protocol-version': '2025-06-18' }), ENV, core, fetcher);
		expect(((await legacyList.json()) as any).result.tools).toHaveLength(3);
	});

	test('unknown method → JSON-RPC -32601', async () => {
		const { res, body } = await rpc('resources/list', {});
		expect(res.status).toBe(200);
		expect(body.error.code).toBe(-32601);
	});
});

describe('protocol', () => {
	test('initialize negotiates the version', async () => {
		const { body } = await rpc('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 't', version: '1' } });
		expect(body.result.protocolVersion).toBe('2025-06-18');
		expect(body.result.capabilities).toEqual({ tools: {} });
		const other = await rpc('initialize', { protocolVersion: '2030-01-01' });
		expect(other.body.result.protocolVersion).toBe('2025-11-25');
	});

	test('tools/list matches the stdio server', async () => {
		const { body } = await rpc('tools/list', {});
		expect(body.result.tools.map((t: { name: string }) => t.name)).toEqual(['convert_schema', 'render_diagram', 'list_formats']);
	});

	test('list_formats comes from the core registry', async () => {
		const { body } = await rpc('tools/call', { name: 'list_formats', arguments: {} });
		expect(body.result.isError).toBe(false);
		expect(body.result.content[0].text.split('\n')).toContain('sql:sqlite');
	});

	test('convert_schema runs the real core and reports fidelity', async () => {
		const { body } = await rpc('tools/call', {
			name: 'convert_schema',
			arguments: { source: 'Table users {\n  id integer [pk]\n  email varchar [unique]\n}\n', to: 'sql' }
		});
		expect(body.result.isError).toBe(false);
		expect(body.result.content[0].text).toContain('CREATE TABLE users');
		expect(body.result.content[0].text).toContain('-- fidelity --\ntables 1/1  columns 2/2');
		expect(body.result.structuredContent.fidelity.idempotent).toBe(true);
	});

	test('render_diagram draws the ERD with the real core', async () => {
		const { body } = await rpc('tools/call', {
			name: 'render_diagram',
			arguments: { source: 'Table users {\n  id integer [pk]\n}\nTable posts {\n  user_id integer\n}\nRef: posts.user_id > users.id\n', theme: 'light' }
		});
		expect(body.result.isError).toBe(false);
		const svg: string = body.result.content[0].text;
		expect(svg.startsWith('<svg xmlns="http://www.w3.org/2000/svg"')).toBe(true);
		expect(svg).toContain('>users<');
		expect(svg).toContain('marker-end');
		expect(body.result.structuredContent.width).toBeGreaterThan(0);
		const bad = await rpc('tools/call', { name: 'render_diagram', arguments: { source: 'x', theme: 'pink' } });
		expect(bad.body.result.isError).toBe(true);
		const undetectable = await rpc('tools/call', { name: 'render_diagram', arguments: { source: 'hello' } });
		expect(undetectable.body.result.isError).toBe(true);
	});

	test('conversion errors are tool errors, not transport errors', async () => {
		const { res, body } = await rpc('tools/call', { name: 'convert_schema', arguments: { source: 'hello', to: 'sql' } });
		expect(res.status).toBe(200);
		expect(body.result.isError).toBe(true);
		const bad = await rpc('tools/call', { name: 'convert_schema', arguments: { source: 1, to: 'sql' } });
		expect(bad.body.result.isError).toBe(true);
		const unknown = await rpc('tools/call', { name: 'drop_database', arguments: {} });
		expect(unknown.body.result.content[0].text).toBe('unknown tool: drop_database');
	});
});

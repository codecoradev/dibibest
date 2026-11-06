// Remote MCP endpoint (epic #52, item 3): Streamable HTTP, stateless, JSON
// responses only (no SSE, no sessions). Every request needs an OAuth access
// token from Gatehouse for this resource; there are no API keys.
//
//   POST /mcp                                     JSON-RPC (initialize, tools/list, tools/call, ping)
//   GET  /.well-known/oauth-protected-resource/mcp  RFC 9728 metadata → Gatehouse
//
// Clients discover Gatehouse from the 401's `resource_metadata`, register by
// Client ID Metadata Document (CIMD), and ask for `resource=<origin>/mcp`.

import { type Env, discover, json } from './auth';
import type { Core, Fidelity } from './core';
import { TokenError, verifyAccessToken } from './jwt';

type Fetch = typeof fetch;

export const MCP_PATH = '/mcp';
export const PRM_PATH = '/.well-known/oauth-protected-resource';
/** Newest first; the first one is offered when the client asks for an unknown version. */
export const PROTOCOL_VERSIONS = ['2025-11-25', '2025-06-18', '2025-03-26'];
/** Request bodies (and so schema sources) above this are refused: keeps a call inside the CPU budget. */
export const MAX_BODY_BYTES = 512 * 1024;
const FORMATS = ['dbml', 'sql', 'sql:mysql', 'sql:sqlite', 'prisma', 'erdjson'];
const VERSION = '0.1.0';

function issuerOf(env: Env): string | null {
	const v = env.OIDC_ISSUER?.trim();
	return v && /^https:\/\//.test(v) ? v.replace(/\/+$/, '') : null;
}

const resourceOf = (req: Request) => `${new URL(req.url).origin}${MCP_PATH}`;
const metadataUrlOf = (req: Request) => `${new URL(req.url).origin}${PRM_PATH}${MCP_PATH}`;

const NOT_CONFIGURED = () => json({ error: 'remote MCP is not configured' }, 404);

/** GET /.well-known/oauth-protected-resource[/mcp] (RFC 9728). Public, so CORS is open. */
export function handleProtectedResource(req: Request, env: Env): Response {
	const issuer = issuerOf(env);
	if (!issuer) return NOT_CONFIGURED();
	return json(
		{
			resource: resourceOf(req),
			authorization_servers: [issuer],
			bearer_methods_supported: ['header'],
			scopes_supported: ['openid', 'profile', 'email', 'offline_access'],
			resource_name: 'dibibest',
			resource_documentation: 'https://github.com/codecoradev/dibibest#mcp-untuk-agen-ai'
		},
		200,
		{ 'access-control-allow-origin': '*', 'cache-control': 'public, max-age=3600' }
	);
}

let asCache: { issuer: string; doc: unknown; at: number } | undefined;

/**
 * GET /.well-known/oauth-authorization-server[/mcp] and /.well-known/openid-configuration
 * on the MCP origin: the issuer's own metadata, unchanged (tokens carry the
 * issuer's `iss`). uteke-cloud serves it there too (its #463); without it the
 * SPA answered these paths with HTML and a 200 (#73).
 */
export async function handleAuthorizationServerMetadata(req: Request, env: Env, fetcher: Fetch = fetch): Promise<Response> {
	const issuer = issuerOf(env);
	if (!issuer) return NOT_CONFIGURED();
	const path = new URL(req.url).pathname.replace(/\/+$/, '');
	const suffix = path.replace(/^\/\.well-known\/(oauth-authorization-server|openid-configuration)/, '');
	if (suffix !== '' && suffix !== MCP_PATH) return json({ error: 'not found' }, 404);
	const fresh = asCache?.issuer === issuer && Date.now() - asCache.at < 300_000 ? asCache.doc : undefined;
	let doc = fresh;
	if (!doc) {
		for (const url of [`${issuer}/.well-known/oauth-authorization-server`, `${issuer}/.well-known/openid-configuration`]) {
			try {
				const res = await fetcher(url, { headers: { accept: 'application/json' } });
				if (!res.ok) continue;
				const v = (await res.json()) as Record<string, unknown>;
				if (typeof v.issuer === 'string' && typeof v.authorization_endpoint === 'string') {
					doc = v;
					asCache = { issuer, doc: v, at: Date.now() };
					break;
				}
			} catch {
				// try the next location
			}
		}
	}
	doc ??= asCache?.issuer === issuer ? asCache.doc : undefined; // stale beats nothing
	if (!doc) return json({ error: 'authorization_server_unavailable' }, 502);
	return json(doc, 200, { 'access-control-allow-origin': '*', 'cache-control': 'public, max-age=300' });
}

function unauthorized(req: Request, error?: string): Response {
	const params = [`resource_metadata="${metadataUrlOf(req)}"`];
	if (error) params.push(`error="${error}"`);
	return json({ error: error ?? 'unauthorized', error_description: 'an OAuth access token for this resource is required' }, 401, {
		'www-authenticate': `Bearer ${params.join(', ')}`
	});
}

type RpcId = string | number | null;
type RpcMessage = { jsonrpc?: string; id?: RpcId; method?: unknown; params?: unknown; result?: unknown; error?: unknown };

const MODERN_META = 'io.modelcontextprotocol/protocolVersion';
/** Stateless revisions served from per-request `_meta` (#70). */
export const MODERN_VERSIONS = ['2026-07-28'];
const TOOLS_TTL_MS = 300_000;
const DISCOVER_TTL_MS = 3_600_000;
const SERVER_INFO = { name: 'dibibest', version: VERSION };

/** The per-request protocol version of a modern request, if any. */
function metaVersion(rpc: RpcMessage): string | undefined {
	const meta = (rpc.params as { _meta?: Record<string, unknown> } | undefined)?._meta;
	const v = meta && typeof meta === 'object' ? meta[MODERN_META] : undefined;
	return typeof v === 'string' ? v : undefined;
}

/** `=?base64?…?=` sentinel of `Mcp-Name` (plain values pass). */
function decodeHeader(raw: string): string | null {
	const m = /^=\?base64\?(.*)\?=$/.exec(raw);
	if (!m) return raw;
	try {
		return new TextDecoder().decode(Uint8Array.from(atob(m[1]), (c) => c.charCodeAt(0)));
	} catch {
		return null;
	}
}

/** Streamable HTTP validation of a modern request: mirrored headers must match the body (-32020). */
function headerMismatch(req: Request, rpc: RpcMessage, version: string): string | null {
	const check = (name: string, expected: string, decode: boolean): string | null => {
		const raw = req.headers.get(name);
		if (raw === null) return `Header mismatch: required header ${name} is missing`;
		const got = decode ? decodeHeader(raw) : raw;
		if (got === null) return `Header mismatch: ${name} header has an invalid base64 value`;
		return got === expected ? null : `Header mismatch: ${name} header does not match the body`;
	};
	const method = String(rpc.method);
	const params = (rpc.params ?? {}) as Record<string, unknown>;
	return (
		check('mcp-protocol-version', version, false) ??
		check('mcp-method', method, false) ??
		(method === 'tools/call' ? check('mcp-name', String(params.name ?? ''), true) : null)
	);
}

const rpcResult = (id: RpcId, result: unknown) => ({ jsonrpc: '2.0', id, result });
const rpcError = (id: RpcId, code: number, message: string) => ({ jsonrpc: '2.0', id, error: { code, message } });

const TOOLS = [
	{
		name: 'convert_schema',
		title: 'Convert schema bolak-balik',
		description:
			'Convert a database schema between formats (dbml, sql, sql:mysql, sql:sqlite, prisma, erdjson; plain sql = PostgreSQL). Returns the converted output plus a fidelity report of what survived.',
		inputSchema: {
			type: 'object',
			properties: {
				source: { type: 'string', description: 'Schema source text (DBML, SQL DDL or Prisma schema)' },
				from: { type: 'string', description: 'Source format id; auto-detected when omitted', enum: FORMATS },
				to: { type: 'string', description: 'Target format id', enum: FORMATS }
			},
			required: ['source', 'to']
		},
		annotations: { readOnlyHint: true, openWorldHint: false }
	},
	{
		name: 'render_diagram',
		title: 'Render the ERD as SVG',
		description:
			'Draw the entity-relationship diagram of a schema as a standalone SVG image (tables, columns with PK/FK, relations). Returns the SVG markup; save it as a .svg file or embed it.',
		inputSchema: {
			type: 'object',
			properties: {
				source: { type: 'string', description: 'Schema source text (DBML, SQL DDL, Prisma schema or ERD-JSON)' },
				from: { type: 'string', description: 'Source format id; auto-detected when omitted', enum: FORMATS },
				theme: { type: 'string', description: 'Colour scheme (default dark)', enum: ['dark', 'light'] }
			},
			required: ['source']
		},
		annotations: { readOnlyHint: true, openWorldHint: false }
	},
	{
		name: 'list_formats',
		title: 'List supported formats',
		description:
			'List format ids the server can convert between, including SQL dialects (sql = PostgreSQL, sql:mysql, sql:sqlite).',
		inputSchema: { type: 'object', properties: {} },
		annotations: { readOnlyHint: true, openWorldHint: false }
	}
];

/** Same text as the stdio server (crates/dibibest-mcp), so both transports read alike. */
function fidelityText(f: Fidelity): string {
	return `tables ${f.tables.kept}/${f.tables.total}  columns ${f.columns.kept}/${f.columns.total}  refs ${f.refs.kept}/${f.refs.total}  indexes ${f.indexes.kept}/${f.indexes.total}  round-trip: ${f.idempotent ? 'lossless' : 'NOT idempotent'}`;
}

const toolError = (text: string) => ({ content: [{ type: 'text', text }], isError: true });

async function callTool(name: unknown, args: Record<string, unknown>, core: Core) {
	if (name === 'list_formats') {
		const r = await core({ op: 'formats' });
		if (!r.ok) return toolError(`error: ${r.error}`);
		return { content: [{ type: 'text', text: (r.formats ?? []).join('\n') }], isError: false };
	}
	if (name === 'render_diagram') {
		const { source, from, theme } = args;
		if (
			typeof source !== 'string' ||
			(from !== undefined && typeof from !== 'string') ||
			(theme !== undefined && theme !== 'dark' && theme !== 'light')
		) {
			return toolError('error: `source` must be a string, `from` a format id and `theme` dark or light');
		}
		const r = await core({ op: 'svg', src: source, from: from ?? 'auto', theme: theme ?? 'dark' });
		if (!r.ok) return toolError(`error: ${r.error}`);
		return {
			content: [{ type: 'text', text: r.svg ?? '' }],
			structuredContent: { svg: r.svg, width: r.width, height: r.height },
			isError: false
		};
	}
	if (name === 'convert_schema') {
		const { source, from, to } = args;
		if (typeof source !== 'string' || typeof to !== 'string' || (from !== undefined && typeof from !== 'string')) {
			return toolError('error: `source` and `to` must be strings (and `from`, when given)');
		}
		const r = await core({ op: 'convert', src: source, from: from ?? 'auto', to });
		if (!r.ok) return toolError(`error: ${r.error}`);
		const output = r.output ?? '';
		return {
			content: [{ type: 'text', text: `${output}\n\n-- fidelity --\n${fidelityText(r.fidelity!)}` }],
			structuredContent: { output, fidelity: r.fidelity },
			isError: false
		};
	}
	return toolError(`unknown tool: ${String(name)}`);
}

async function dispatch(msg: RpcMessage, core: Core): Promise<object> {
	const id = msg.id ?? null;
	const params = (msg.params && typeof msg.params === 'object' ? msg.params : {}) as Record<string, unknown>;
	switch (msg.method) {
		case 'initialize': {
			const asked = params.protocolVersion;
			const version = typeof asked === 'string' && PROTOCOL_VERSIONS.includes(asked) ? asked : PROTOCOL_VERSIONS[0];
			return rpcResult(id, {
				protocolVersion: version,
				capabilities: { tools: {} },
				serverInfo: { name: 'dibibest', title: 'dibibest — bidirectional schema conversion', version: VERSION }
			});
		}
		case 'ping':
			return rpcResult(id, {});
		case 'tools/list':
			return rpcResult(id, { tools: TOOLS });
		case 'tools/call': {
			const args = params.arguments && typeof params.arguments === 'object' ? params.arguments : {};
			return rpcResult(id, await callTool(params.name, args as Record<string, unknown>, core));
		}
		default:
			return rpcError(id, -32601, `method not found: ${String(msg.method)}`);
	}
}

/** A modern (2026-07-28) request: stateless, results carry `resultType` and `serverInfo`. */
async function modern(req: Request, rpc: RpcMessage, version: string | undefined, core: Core): Promise<Response> {
	const id = rpc.id ?? null;
	const err = (status: number, code: number, message: string, data?: unknown) => {
		console.log(`mcp: ${rpc.method} (modern) -> ${code}`);
		return json({ jsonrpc: '2.0', id, error: data === undefined ? { code, message } : { code, message, data } }, status);
	};
	if (version !== undefined) {
		const mismatch = headerMismatch(req, rpc, version);
		if (mismatch) return err(400, -32020, mismatch);
		if (!MODERN_VERSIONS.includes(version)) {
			return err(400, -32022, 'Unsupported protocol version', {
				supported: [...MODERN_VERSIONS, ...PROTOCOL_VERSIONS],
				requested: version
			});
		}
	}
	const params = (rpc.params && typeof rpc.params === 'object' ? rpc.params : {}) as Record<string, unknown>;
	let result: Record<string, unknown>;
	switch (rpc.method) {
		case 'server/discover':
			result = {
				supportedVersions: [...MODERN_VERSIONS, ...PROTOCOL_VERSIONS],
				capabilities: { tools: {} },
				ttlMs: DISCOVER_TTL_MS,
				cacheScope: 'public'
			};
			break;
		case 'tools/list':
			// private like uteke-cloud: the response belongs to an authorised caller
			result = { tools: TOOLS, ttlMs: TOOLS_TTL_MS, cacheScope: 'private' };
			break;
		case 'tools/call': {
			const args = params.arguments && typeof params.arguments === 'object' ? params.arguments : {};
			result = (await callTool(params.name, args as Record<string, unknown>, core)) as Record<string, unknown>;
			break;
		}
		default:
			// ping and the initialize handshake are gone in the modern revision
			return err(404, -32601, `method not found: ${String(rpc.method)}`);
	}
	console.log(`mcp: ${rpc.method} (modern)`);
	return json(rpcResult(id, { ...result, resultType: 'complete', _meta: { 'io.modelcontextprotocol/serverInfo': SERVER_INFO } }));
}

const CORS_ALLOW_HEADERS =
	'authorization, content-type, accept, mcp-protocol-version, mcp-session-id, mcp-method, mcp-name, last-event-id';

/**
 * CORS for MCP clients that call from a browser origin (claude.ai sends
 * `Origin: https://claude.ai`). Every request needs a bearer token and no
 * cookie is read, so a cross-origin page cannot act for a user. That is why
 * any origin is echoed instead of refused (a 403 here broke the Claude
 * connector, #68).
 */
function withCors(req: Request, res: Response): Response {
	const origin = req.headers.get('origin');
	if (!origin) return res;
	const headers = new Headers(res.headers);
	headers.set('access-control-allow-origin', origin);
	headers.set('access-control-expose-headers', 'www-authenticate, mcp-session-id, mcp-protocol-version');
	headers.append('vary', 'origin');
	return new Response(res.body, { status: res.status, statusText: res.statusText, headers });
}

/** /mcp: CORS preflight, then the MCP transport. */
export async function handleMcp(req: Request, env: Env, core: Core, fetcher: Fetch = fetch): Promise<Response> {
	if (!issuerOf(env)) return NOT_CONFIGURED();
	if (req.method === 'OPTIONS') {
		return withCors(
			req,
			new Response(null, {
				status: 204,
				headers: {
					'access-control-allow-methods': 'POST, GET, DELETE, OPTIONS',
					'access-control-allow-headers': CORS_ALLOW_HEADERS,
					'access-control-max-age': '86400'
				}
			})
		);
	}
	return withCors(req, await serve(req, env, core, fetcher));
}

/** POST /mcp (and 405 for GET/DELETE: no server-initiated stream, no sessions). */
async function serve(req: Request, env: Env, core: Core, fetcher: Fetch): Promise<Response> {
	const issuer = issuerOf(env)!;
	if (req.method !== 'POST') return json({ error: 'method not allowed' }, 405, { allow: 'POST, OPTIONS' });

	const auth = req.headers.get('authorization') ?? '';
	const m = /^Bearer\s+([A-Za-z0-9._~+/=-]+)$/i.exec(auth.trim());
	if (!m) {
		console.log(`mcp: 401 no bearer ua=${(req.headers.get('user-agent') ?? '').slice(0, 40)}`);
		return unauthorized(req);
	}
	let jwksUri: string | undefined;
	try {
		jwksUri = (await discover(issuer, fetcher)).jwks_uri;
	} catch {
		jwksUri = undefined;
	}
	if (!jwksUri) return json({ error: 'the identity provider cannot be reached right now' }, 503, { 'retry-after': '30' });
	try {
		await verifyAccessToken(m[1], { issuer, audience: resourceOf(req), jwksUri, fetcher });
	} catch (e) {
		if (e instanceof TokenError) {
			// visible in the Cloudflare real-time logs; never logs the token
			console.warn(`mcp: token refused: ${e.message}`);
			return unauthorized(req, 'invalid_token');
		}
		return json({ error: 'the identity provider cannot be reached right now' }, 503, { 'retry-after': '30' });
	}

	// The version is negotiated in `initialize` (legacy MCP, 2025-11-25 and
	// earlier). Modern requests (2026-07-28) are answered below with a 4xx, which
	// is the spec's signal for a dual-era client such as Claude to fall back.
	const length = Number(req.headers.get('content-length') ?? 0);
	if (length > MAX_BODY_BYTES) return json(rpcError(null, -32600, 'request too large'), 413);
	const text = await req.text();
	if (new TextEncoder().encode(text).length > MAX_BODY_BYTES) return json(rpcError(null, -32600, 'request too large'), 413);

	let msg: unknown;
	try {
		msg = JSON.parse(text);
	} catch {
		return json(rpcError(null, -32700, 'parse error'), 400);
	}
	if (Array.isArray(msg)) return json(rpcError(null, -32600, 'batching is not supported'), 400);
	if (!msg || typeof msg !== 'object' || (msg as RpcMessage).jsonrpc !== '2.0') {
		return json(rpcError(null, -32600, 'invalid request'), 400);
	}
	const rpc = msg as RpcMessage;
	// notifications (no id) and client responses get no body
	// who is calling and how (no token, no body): the connector handshake is
	// then readable in the real-time logs
	console.log(
		`mcp: <- ${typeof rpc.method === 'string' ? rpc.method : rpc.id !== undefined ? '(response)' : '?'}` +
			` ua=${(req.headers.get('user-agent') ?? '').slice(0, 40)}` +
			` pv=${req.headers.get('mcp-protocol-version') ?? '-'} mm=${req.headers.get('mcp-method') ?? '-'}` +
			` accept=${(req.headers.get('accept') ?? '-').slice(0, 60)}`
	);
	if (rpc.method === undefined || rpc.id === undefined) return new Response(null, { status: 202 });
	if (typeof rpc.method !== 'string') return json(rpcError(rpc.id ?? null, -32600, 'invalid request'), 400);
	// Era selection, as uteke-cloud does (#70): `initialize` = legacy handshake;
	// a per-request `_meta` version, or `server/discover`, = modern and stateless
	// (MCP 2026-07-28). Claude opens with `server/discover` and stays modern.
	const version = metaVersion(rpc);
	if (rpc.method !== 'initialize' && (version !== undefined || rpc.method === 'server/discover')) {
		return modern(req, rpc, version, core);
	}
	const reply = (await dispatch(rpc, core)) as { error?: { code: number } };
	console.log(`mcp: ${rpc.method}${reply.error ? ` -> ${reply.error.code}` : ''}`);
	return json(reply);
}

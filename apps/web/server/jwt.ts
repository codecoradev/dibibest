// Access-token verification for the remote MCP endpoint (epic #52, item 3).
// Gatehouse issues RFC 9068 JWT access tokens when the client names a resource
// (RFC 8707); we verify them locally against the issuer's JWKS, so no call to
// the identity provider is needed per request.

import { b64urlDecode, nowS } from './auth';

type Jwk = JsonWebKey & { kid?: string; alg?: string; use?: string };

const ALGS: Record<string, { import: RsaHashedImportParams | EcKeyImportParams; verify: AlgorithmIdentifier | EcdsaParams }> = {
	RS256: { import: { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, verify: { name: 'RSASSA-PKCS1-v1_5' } },
	PS256: { import: { name: 'RSA-PSS', hash: 'SHA-256' }, verify: { name: 'RSA-PSS', saltLength: 32 } as AlgorithmIdentifier },
	ES256: { import: { name: 'ECDSA', namedCurve: 'P-256' }, verify: { name: 'ECDSA', hash: 'SHA-256' } }
};

/** Clock skew tolerated on `exp`/`nbf`/`iat`, in seconds. */
const LEEWAY_S = 60;
const JWKS_TTL_MS = 3600_000;
/** An unknown `kid` refetches the JWKS (key rotation), at most this often. */
const JWKS_REFETCH_MS = 60_000;

let jwksCache: { uri: string; keys: Jwk[]; at: number } | undefined;

/** Test hook: forget the cached JWKS. */
export function resetJwksCache() {
	jwksCache = undefined;
}

async function jwks(uri: string, fetcher: typeof fetch, refresh: boolean): Promise<Jwk[]> {
	const fresh = jwksCache?.uri === uri && Date.now() - jwksCache.at < (refresh ? JWKS_REFETCH_MS : JWKS_TTL_MS);
	if (fresh) return jwksCache!.keys;
	const res = await fetcher(uri, { headers: { accept: 'application/json' } });
	if (!res.ok) throw new Error(`jwks fetch failed: ${res.status}`);
	const doc = (await res.json()) as { keys?: Jwk[] };
	if (!Array.isArray(doc.keys)) throw new Error('jwks has no keys');
	jwksCache = { uri, keys: doc.keys, at: Date.now() };
	return doc.keys;
}

export type AccessClaims = {
	iss: string;
	sub: string;
	aud: string | string[];
	exp: number;
	nbf?: number;
	iat?: number;
	scope?: string;
	client_id?: string;
	azp?: string;
	cnf?: unknown;
};

/** Why a token was refused (never shown verbatim to clients beyond `invalid_token`). */
export class TokenError extends Error {}

function decodeJson<T>(part: string): T {
	const raw = b64urlDecode(part);
	if (!raw) throw new TokenError('malformed token');
	try {
		return JSON.parse(new TextDecoder().decode(raw)) as T;
	} catch {
		throw new TokenError('malformed token');
	}
}

/**
 * The token's claims when it is a JWT signed by a key in the issuer's JWKS, issued
 * by `issuer`, meant for `audience` (the MCP resource URL), and currently valid.
 * Throws TokenError otherwise; other errors mean the JWKS could not be fetched.
 */
export async function verifyAccessToken(
	token: string,
	opts: { issuer: string; audience: string; jwksUri: string; fetcher?: typeof fetch; now?: number }
): Promise<AccessClaims> {
	const fetcher = opts.fetcher ?? fetch;
	const parts = token.split('.');
	if (parts.length !== 3) throw new TokenError('not a JWT');
	const [h, p, s] = parts;
	const header = decodeJson<{ alg?: string; kid?: string; typ?: string; crit?: unknown }>(h);
	const alg = header.alg && Object.hasOwn(ALGS, header.alg) ? ALGS[header.alg] : undefined;
	if (!alg) throw new TokenError(`unsupported alg ${header.alg}`);
	if (header.crit !== undefined) throw new TokenError('unsupported crit header');
	const sig = b64urlDecode(s);
	if (!sig) throw new TokenError('malformed signature');

	const pick = (keys: Jwk[]) =>
		keys.filter(
			(k) =>
				(header.kid === undefined || k.kid === header.kid) &&
				(k.alg === undefined || k.alg === header.alg) &&
				(k.use === undefined || k.use === 'sig')
		);
	let candidates = pick(await jwks(opts.jwksUri, fetcher, false));
	if (candidates.length === 0) candidates = pick(await jwks(opts.jwksUri, fetcher, true));
	if (candidates.length === 0) throw new TokenError('no matching key');

	const data = new TextEncoder().encode(`${h}.${p}`);
	let ok = false;
	for (const jwk of candidates) {
		try {
			const { kid: _k, alg: _a, use: _u, key_ops: _o, ...material } = jwk;
			const key = await crypto.subtle.importKey('jwk', material, alg.import, false, ['verify']);
			if (await crypto.subtle.verify(alg.verify, key, sig, data)) {
				ok = true;
				break;
			}
		} catch {
			// a key of another type: try the next one
		}
	}
	if (!ok) throw new TokenError('bad signature');

	const c = decodeJson<Partial<AccessClaims>>(p);
	const now = opts.now ?? nowS();
	if (c.iss !== opts.issuer) throw new TokenError('wrong issuer');
	const aud = Array.isArray(c.aud) ? c.aud : [c.aud];
	if (!aud.includes(opts.audience)) throw new TokenError('wrong audience');
	if (typeof c.exp !== 'number' || c.exp + LEEWAY_S <= now) throw new TokenError('expired');
	if (typeof c.nbf === 'number' && c.nbf - LEEWAY_S > now) throw new TokenError('not yet valid');
	if (typeof c.iat === 'number' && c.iat - LEEWAY_S > now) throw new TokenError('issued in the future');
	if (typeof c.sub !== 'string' || !c.sub) throw new TokenError('no subject');
	// DPoP-bound tokens (RFC 9449) need a proof we do not verify: refuse them as bearer tokens.
	if (c.cnf !== undefined) throw new TokenError('sender-constrained token');
	return c as AccessClaims;
}

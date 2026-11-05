// Bridge to dibibest-core (compiled to WASM by crates/dibibest-wasm). The only
// place that knows the C ABI: alloc / dealloc / run, JSON in, JSON out.
import wasmUrl from './wasm/dibibest.wasm?url';

export type Outcome = { kept: number; total: number };
/** Mirror of `dibibest_core::Fidelity` — rendered as-is, never recomputed. */
export type Fidelity = {
	tables: Outcome;
	columns: Outcome;
	refs: Outcome;
	indexes: Outcome;
	dropped?: string[];
	idempotent: boolean;
};

export type ErdColumn = {
	name: string;
	type: string;
	isPk: boolean;
	isFk: boolean;
	isUnique: boolean;
	isNullable: boolean;
	autoincrement?: boolean;
	default?: string;
	note?: string;
};
export type ErdNode = {
	id: string;
	name: string;
	position: { x: number; y: number } | null;
	columns: ErdColumn[];
	note?: string;
	indexes?: unknown[];
};
export type RefAction = 'cascade' | 'set null' | 'set default' | 'restrict' | 'no action';

export type ErdEdge = {
	id: string;
	from: { table: string; columns: string[] };
	to: { table: string; columns: string[] };
	kind: 'many-to-one' | 'one-to-one' | 'one-to-many';
	label?: string;
	onDelete?: RefAction;
	onUpdate?: RefAction;
};
/** ERD-JSON v1 (docs/erdjson-schema.md) — the single workspace state. */
export type ErdDoc = {
	version: number;
	nodes: ErdNode[];
	edges: ErdEdge[];
	enums?: unknown[];
	meta: { layout: string | null; sourceFormat?: string };
};

/** Data payload of a canvas table node. */
export type TableData = { node: ErdNode; tone: 'normal' | 'related' | 'dimmed'; editable: boolean };

export type Result = { ok: true; output: string; fidelity: Fidelity; ms: number } | { ok: false; error: string };

type Exports = {
	memory: WebAssembly.Memory;
	alloc(len: number): number;
	dealloc(ptr: number, len: number): void;
	run(ptr: number, len: number): number;
};

let wasm: Promise<Exports> | undefined;

function load(): Promise<Exports> {
	wasm ??= WebAssembly.instantiateStreaming(fetch(wasmUrl), {})
		.catch(async () => WebAssembly.instantiate(await (await fetch(wasmUrl)).arrayBuffer(), {}))
		.then((r) => r.instance.exports as unknown as Exports);
	return wasm;
}

const enc = new TextEncoder();
const dec = new TextDecoder();

async function call(req: object): Promise<Result> {
	const w = await load();
	const t0 = performance.now();
	const bytes = enc.encode(JSON.stringify(req));
	const inPtr = w.alloc(bytes.length);
	new Uint8Array(w.memory.buffer, inPtr, bytes.length).set(bytes); // re-read buffer: alloc may grow memory
	const outPtr = w.run(inPtr, bytes.length);
	w.dealloc(inPtr, bytes.length);
	const len = new DataView(w.memory.buffer).getUint32(outPtr, true);
	const res = JSON.parse(dec.decode(new Uint8Array(w.memory.buffer, outPtr + 4, len)));
	w.dealloc(outPtr, 4 + len);
	return res.ok ? { ...res, ms: performance.now() - t0 } : res;
}

/** source text → ERD-JSON (grid-laid-out) + fidelity of the conversion. */
export const toErd = (src: string, from: string) => call({ op: 'erd', src, from });
/** source text → another format + fidelity. */
export const convert = (src: string, from: string, to: string) => call({ op: 'convert', src, from, to });

// The dibibest core (crates/dibibest-wasm) inside a Pages Function: same C ABI
// as src/lib/core.ts, but the module is precompiled at deploy time (a `.wasm`
// import is a WebAssembly.Module in Workers), so instantiating it is cheap.

type Exports = {
	memory: WebAssembly.Memory;
	alloc(len: number): number;
	dealloc(ptr: number, len: number): void;
	run(ptr: number, len: number): number;
};

export type Fidelity = {
	tables: { kept: number; total: number };
	columns: { kept: number; total: number };
	refs: { kept: number; total: number };
	indexes: { kept: number; total: number };
	dropped?: string[];
	idempotent: boolean;
};

export type CoreReply =
	| { ok: true; output?: string; fidelity?: Fidelity; from?: string; formats?: string[]; svg?: string; width?: number; height?: number }
	| { ok: false; error: string };

export type Core = (req: Record<string, unknown>) => Promise<CoreReply>;

const enc = new TextEncoder();
const dec = new TextDecoder();

/** A JSON-in/JSON-out caller over a compiled core module (instantiated once, on first use). */
export function makeCore(module: WebAssembly.Module): Core {
	let instance: Promise<Exports> | undefined;
	return async (req) => {
		instance ??= WebAssembly.instantiate(module, {}).then((i) => i.exports as unknown as Exports);
		const w = await instance;
		const bytes = enc.encode(JSON.stringify(req));
		const inPtr = w.alloc(bytes.length);
		new Uint8Array(w.memory.buffer, inPtr, bytes.length).set(bytes);
		const outPtr = w.run(inPtr, bytes.length);
		w.dealloc(inPtr, bytes.length);
		const len = new DataView(w.memory.buffer).getUint32(outPtr, true);
		const reply = JSON.parse(dec.decode(new Uint8Array(w.memory.buffer, outPtr + 4, len))) as CoreReply;
		w.dealloc(outPtr, 4 + len);
		return reply;
	};
}

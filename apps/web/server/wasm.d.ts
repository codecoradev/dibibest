// Workers (and wrangler's Pages Functions bundler) import `.wasm` files as compiled modules.
declare module '*.wasm' {
	const module: WebAssembly.Module;
	export default module;
}

// The ERD as an image (#60). The SVG is drawn by the core (dibibest_core::diagram,
// through WASM), the same renderer the MCP servers and the CLI use (#64). Only the
// PNG rasterisation lives here, because it needs the browser's canvas.

export type DiagramSvg = { svg: string; width: number; height: number };

/** Largest side of the PNG: browsers refuse canvases much beyond ~16k px. */
const MAX_PX = 16000;

/** Rasterise the SVG to a PNG blob at `scale` (2 = retina), clamped to the canvas limit. */
export async function svgToPng(d: DiagramSvg, scale = 2): Promise<Blob> {
	const k = Math.min(scale, MAX_PX / Math.max(d.width, d.height));
	const url = URL.createObjectURL(new Blob([d.svg], { type: 'image/svg+xml;charset=utf-8' }));
	try {
		const img = new Image();
		img.decoding = 'async';
		img.src = url;
		await img.decode();
		const canvas = document.createElement('canvas');
		canvas.width = Math.round(d.width * k);
		canvas.height = Math.round(d.height * k);
		const ctx = canvas.getContext('2d');
		if (!ctx) throw new Error('canvas is not available');
		ctx.scale(k, k);
		ctx.drawImage(img, 0, 0, d.width, d.height);
		return await new Promise<Blob>((ok, fail) =>
			canvas.toBlob((b) => (b ? ok(b) : fail(new Error('could not encode the PNG'))), 'image/png')
		);
	} finally {
		URL.revokeObjectURL(url);
	}
}

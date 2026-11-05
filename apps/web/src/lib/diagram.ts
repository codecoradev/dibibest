// The ERD as an image (#60). Drawn from the workspace state (ERD-JSON), not by
// screenshotting the canvas, so it works whether or not the canvas is on screen
// (phones show one pane at a time) and the output is the same everywhere.
// Geometry mirrors TableNode.svelte: fixed 220px cards, 33px header, 24px rows.

import type { ErdDoc, ErdNode } from './core';

export type Theme = 'dark' | 'light';

const W = 220;
const HEADER = 33;
const ROW = 24;
const PAD_Y = 4;
const MARGIN = 40;
/** Grid used by the core when a table has no position yet (GRID_CELL_W/H). */
const CELL_W = 320;
const CELL_H = 360;

const THEMES = {
	dark: {
		bg: '#0b0d10',
		card: '#171b23',
		head: '#1b202a',
		border: '#262d3a',
		line: '#1c222d',
		text: '#e8ecf2',
		text2: '#9aa4b2',
		text3: '#8b95a5',
		accent: '#4f8cff',
		pk: '#f5b544',
		fk: '#b48cff',
		edge: '#b48cff'
	},
	light: {
		bg: '#ffffff',
		card: '#ffffff',
		head: '#f4f6fa',
		border: '#d5dbe5',
		line: '#e5e9f0',
		text: '#111827',
		text2: '#374151',
		text3: '#6b7280',
		accent: '#2563eb',
		pk: '#b7791f',
		fk: '#7c3aed',
		edge: '#7c3aed'
	}
} as const;

const SANS = "'IBM Plex Sans', system-ui, -apple-system, 'Segoe UI', sans-serif";
const MONO = "'JetBrains Mono', ui-monospace, 'SFMono-Regular', Menlo, Consolas, monospace";

export function esc(s: string): string {
	return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
}

/** Cut `s` to `max` characters with an ellipsis (the card has a fixed width). */
function fit(s: string, max: number): string {
	return s.length <= max ? s : `${s.slice(0, Math.max(1, max - 1))}…`;
}

const height = (n: ErdNode) => 1 + HEADER + PAD_Y + n.columns.length * ROW + PAD_Y + 1;

/** xyflow's default bezier between a right-side source and a left-side target handle. */
function bezier(sx: number, sy: number, tx: number, ty: number): string {
	const off = (d: number) => (d >= 0 ? 0.5 * d : 0.25 * 25 * Math.sqrt(-d));
	const c1 = sx + off(tx - sx);
	const c2 = tx - off(tx - sx);
	return `M${sx},${sy} C${c1},${sy} ${c2},${ty} ${tx},${ty}`;
}

export type DiagramSvg = { svg: string; width: number; height: number };

/**
 * The diagram as a standalone SVG document. `hidden` leaves tables out (Explorer).
 * Returns null when there is nothing to draw.
 */
export function diagramSvg(erd: ErdDoc, opts: { theme?: Theme; hidden?: ReadonlySet<string> } = {}): DiagramSvg | null {
	const t = THEMES[opts.theme ?? 'dark'];
	const nodes = erd.nodes.filter((n) => !opts.hidden?.has(n.id));
	if (!nodes.length) return null;

	const pos = new Map<string, { x: number; y: number }>();
	nodes.forEach((n, i) => {
		const cols = Math.max(1, Math.ceil(Math.sqrt(nodes.length)));
		pos.set(n.id, n.position ?? { x: (i % cols) * CELL_W, y: Math.floor(i / cols) * CELL_H });
	});

	let minX = Infinity;
	let minY = Infinity;
	let maxX = -Infinity;
	let maxY = -Infinity;
	for (const n of nodes) {
		const p = pos.get(n.id)!;
		minX = Math.min(minX, p.x);
		minY = Math.min(minY, p.y);
		maxX = Math.max(maxX, p.x + W);
		maxY = Math.max(maxY, p.y + height(n));
	}
	const ox = MARGIN - minX;
	const oy = MARGIN - minY;
	const width = Math.ceil(maxX - minX + 2 * MARGIN);
	const h = Math.ceil(maxY - minY + 2 * MARGIN + 16);

	const byId = new Map(nodes.map((n) => [n.id, n]));
	const rowY = (n: ErdNode, col: string) => {
		const i = Math.max(0, n.columns.findIndex((c) => c.name === col));
		return pos.get(n.id)!.y + oy + 1 + HEADER + PAD_Y + i * ROW + ROW / 2;
	};

	const edges: string[] = [];
	for (const e of erd.edges) {
		const s = byId.get(e.from.table);
		const d = byId.get(e.to.table);
		if (!s || !d) continue;
		const sx = pos.get(s.id)!.x + ox + W;
		const tx = pos.get(d.id)!.x + ox;
		edges.push(
			`<path d="${bezier(sx, rowY(s, e.from.columns[0]), tx, rowY(d, e.to.columns[0]))}" fill="none" stroke="${t.edge}" stroke-width="1.6" marker-end="url(#arrow)"/>`
		);
	}

	const cards = nodes.map((n) => {
		const p = pos.get(n.id)!;
		const x = p.x + ox;
		const y = p.y + oy;
		const hh = height(n);
		const rows = n.columns
			.map((c, i) => {
				const cy = y + 1 + HEADER + PAD_Y + i * ROW + ROW / 2;
				const key = c.isPk ? 'PK' : c.isFk ? 'FK' : '';
				const type = fit(c.type, 14);
				const name = fit(c.name, Math.max(6, 24 - type.length));
				return [
					key
						? `<text x="${x + 20}" y="${cy}" fill="${c.isPk ? t.pk : t.fk}" font-size="10.5" font-weight="700" text-anchor="middle" dominant-baseline="central">${key}</text>`
						: '',
					`<text x="${x + 36}" y="${cy}" fill="${t.text2}" font-size="12" dominant-baseline="central">${esc(name)}</text>`,
					`<text x="${x + W - 12}" y="${cy}" fill="${t.text3}" font-size="11" text-anchor="end" dominant-baseline="central">${esc(type)}</text>`,
					`<circle cx="${x}" cy="${cy}" r="3" fill="${t.fk}"/><circle cx="${x + W}" cy="${cy}" r="3" fill="${t.fk}"/>`
				].join('');
			})
			.join('');
		return [
			`<g>`,
			`<rect x="${x + 0.5}" y="${y + 0.5}" width="${W - 1}" height="${hh - 1}" rx="10" fill="${t.card}" stroke="${t.border}"/>`,
			`<path d="M${x + 1},${y + 1 + HEADER} H${x + W - 1} V${y + 10} a9,9 0 0 0 -9,-9 H${x + 10} a9,9 0 0 0 -9,9 Z" fill="${t.head}"/>`,
			`<line x1="${x + 1}" y1="${y + 1 + HEADER}" x2="${x + W - 1}" y2="${y + 1 + HEADER}" stroke="${t.line}"/>`,
			`<rect x="${x + 12}" y="${y + 13}" width="9" height="9" rx="2" fill="none" stroke="${t.accent}" stroke-width="1.5"/>`,
			`<text x="${x + 28}" y="${y + 1 + HEADER / 2}" fill="${t.text}" font-family="${SANS}" font-size="12.5" font-weight="650" dominant-baseline="central">${esc(fit(n.name, 22))}</text>`,
			`<text x="${x + W - 12}" y="${y + 1 + HEADER / 2}" fill="${t.text3}" font-family="${SANS}" font-size="11" text-anchor="end" dominant-baseline="central">${n.columns.length}c</text>`,
			`<g font-family="${MONO}">${rows}</g>`,
			`</g>`
		].join('');
	});

	const svg = [
		`<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${h}" viewBox="0 0 ${width} ${h}">`,
		`<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="${t.edge}"/></marker></defs>`,
		`<rect width="100%" height="100%" fill="${t.bg}"/>`,
		edges.join(''),
		cards.join(''),
		`<text x="${width - 12}" y="${h - 12}" fill="${t.text3}" font-family="${SANS}" font-size="10" text-anchor="end" opacity="0.7">dibibest</text>`,
		`</svg>`
	].join('');
	return { svg, width, height: h };
}

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

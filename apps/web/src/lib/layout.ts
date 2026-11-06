// Placing tables that are new to the canvas (#75). Tables already on the canvas
// keep their positions; the core's grid gives new ones positions computed as if
// the canvas were empty, which can land on top of a table that stayed. New
// tables go to the first free grid cell instead.

import type { ErdNode } from './core';

/** Same cell as the core grid (erdjson::GRID_CELL_W/H) and the table card width. */
const CELL_W = 320;
const CELL_H = 360;
const CARD_W = 220;
const GAP = 40;

const height = (n: ErdNode) => 1 + 33 + 8 + n.columns.length * 24 + 1;

type Rect = { x: number; y: number; w: number; h: number };
const overlaps = (a: Rect, b: Rect) =>
	a.x < b.x + b.w + GAP && b.x < a.x + a.w + GAP && a.y < b.y + b.h + GAP && b.y < a.y + a.h + GAP;

/**
 * Give every node in `nodes` a position: the one in `kept` when it has one,
 * otherwise the first grid cell (row-major) that does not overlap any placed table.
 */
export function placeNodes(nodes: ErdNode[], kept: Map<string, ErdNode['position']>): void {
	const placed: Rect[] = [];
	const fresh: ErdNode[] = [];
	for (const n of nodes) {
		const p = kept.get(n.id);
		if (p) {
			n.position = p;
			placed.push({ x: p.x, y: p.y, w: CARD_W, h: height(n) });
		} else {
			fresh.push(n);
		}
	}
	if (!fresh.length) return;
	if (!placed.length) return; // a fresh canvas: the core grid is already tidy
	const cols = Math.max(1, Math.ceil(Math.sqrt(nodes.length)));
	let cell = 0;
	for (const n of fresh) {
		for (; ; cell++) {
			const r = { x: (cell % cols) * CELL_W, y: Math.floor(cell / cols) * CELL_H, w: CARD_W, h: height(n) };
			if (!placed.some((q) => overlaps(r, q))) {
				n.position = { x: r.x, y: r.y };
				placed.push(r);
				cell++;
				break;
			}
		}
	}
}

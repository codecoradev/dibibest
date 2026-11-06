import { expect, test } from 'bun:test';
import type { ErdDoc } from '../src/lib/core';
import { sanitizeErd } from '../src/lib/layout';

const col = (name: string) => ({ name, type: 'int', isPk: false, isFk: false, isUnique: false, isNullable: true });

test('a stored diagram with duplicates keeps the first of each instead of crashing the canvas', () => {
	const doc = {
		version: 1,
		nodes: [
			{ id: 'z', name: 'z', position: { x: 10, y: 20 }, columns: [col('id'), col('id'), col('x')] },
			{ id: 'z', name: 'z', position: null, columns: [col('y')] },
			{ id: 'u', name: 'u', position: null, columns: [col('id')] }
		],
		edges: [
			{ id: 'e1', from: { table: 'z', columns: ['x'] }, to: { table: 'u', columns: ['id'] }, kind: 'many-to-one' },
			{ id: 'e1', from: { table: 'z', columns: ['x'] }, to: { table: 'u', columns: ['id'] }, kind: 'many-to-one' },
			{ id: 'e2', from: { table: 'gone', columns: ['x'] }, to: { table: 'u', columns: ['id'] }, kind: 'many-to-one' }
		],
		meta: { layout: null }
	} as unknown as ErdDoc;
	const out = sanitizeErd(doc)!;
	expect(out.nodes.map((n) => n.id)).toEqual(['z', 'u']);
	expect(out.nodes[0].columns.map((c) => c.name)).toEqual(['id', 'x']);
	expect(out.nodes[0].position).toEqual({ x: 10, y: 20 });
	expect(out.edges.map((e) => e.id)).toEqual(['e1']);
});

test('nothing usable gives null', () => {
	expect(sanitizeErd(null)).toBeNull();
	expect(sanitizeErd({} as ErdDoc)).toBeNull();
});

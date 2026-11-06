import { expect, test } from 'bun:test';
import type { ErdNode } from '../src/lib/core';
import { placeNodes } from '../src/lib/layout';

const node = (id: string, cols = 3, position: ErdNode['position'] = null): ErdNode => ({
	id,
	name: id,
	position,
	columns: Array.from({ length: cols }, (_, i) => ({ name: `c${i}`, type: 'int', isPk: false, isFk: false, isUnique: false, isNullable: true }))
});

test('a new table never lands on a table that kept its position (#75)', () => {
	// users and posts were on the canvas; follows is new and comes first in the
	// source, so the core grid would put it at (0, 0), right on top of users
	const nodes = [node('follows', 3, { x: 0, y: 0 }), node('users', 4, { x: 320, y: 0 }), node('posts', 6, { x: 640, y: 0 })];
	const kept = new Map<string, ErdNode['position']>([
		['users', { x: 0, y: 0 }],
		['posts', { x: 320, y: 0 }]
	]);
	placeNodes(nodes, kept);
	const [follows, users, posts] = nodes;
	expect(users.position).toEqual({ x: 0, y: 0 });
	expect(posts.position).toEqual({ x: 320, y: 0 });
	expect(follows.position).not.toEqual({ x: 0, y: 0 });
	expect(follows.position).not.toEqual({ x: 320, y: 0 });
	expect(follows.position!.x === 0 && follows.position!.y === 0).toBe(false);
});

test('a fresh canvas keeps the core grid', () => {
	const nodes = [node('a', 2, { x: 0, y: 0 }), node('b', 2, { x: 320, y: 0 })];
	placeNodes(nodes, new Map());
	expect(nodes.map((n) => n.position)).toEqual([
		{ x: 0, y: 0 },
		{ x: 320, y: 0 }
	]);
});

test('several new tables get distinct free cells', () => {
	const nodes = [node('x'), node('y'), node('kept', 3, null), node('z')];
	placeNodes(nodes, new Map([['kept', { x: 0, y: 0 }]]));
	const spots = nodes.map((n) => `${n.position!.x},${n.position!.y}`);
	expect(new Set(spots).size).toBe(4);
	expect(spots.filter((s) => s === '0,0')).toEqual(['0,0']); // only `kept`
});

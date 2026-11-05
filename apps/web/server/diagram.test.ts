import { describe, expect, test } from 'bun:test';
import type { ErdDoc } from '../src/lib/core';
import { diagramSvg, esc } from '../src/lib/diagram';

const col = (name: string, type: string, extra: Partial<ErdDoc['nodes'][0]['columns'][0]> = {}) => ({
	name,
	type,
	isPk: false,
	isFk: false,
	isUnique: false,
	isNullable: true,
	...extra
});

const ERD: ErdDoc = {
	version: 1,
	nodes: [
		{ id: 'users', name: 'users', position: { x: 0, y: 0 }, columns: [col('id', 'integer', { isPk: true }), col('email', 'varchar')] },
		{
			id: 'orders',
			name: 'orders',
			position: { x: 320, y: 40 },
			columns: [col('id', 'integer', { isPk: true }), col('user_id', 'integer', { isFk: true })]
		},
		{ id: 'odd', name: 'a<b>&"c"', position: null, columns: [col('x', 'text')] }
	],
	edges: [
		{ id: 'e1', from: { table: 'orders', columns: ['user_id'] }, to: { table: 'users', columns: ['id'] }, kind: 'many-to-one' }
	],
	meta: { layout: 'grid' }
};

describe('diagramSvg', () => {
	test('draws every table, column and ref, fitted to the content', () => {
		const d = diagramSvg(ERD)!;
		expect(d.svg.startsWith('<svg xmlns="http://www.w3.org/2000/svg"')).toBe(true);
		for (const s of ['>users<', '>orders<', '>email<', '>user_id<', '>PK<', '>FK<', '>varchar<']) expect(d.svg).toContain(s);
		expect(d.svg.match(/marker-end="url\(#arrow\)"/g)).toHaveLength(1);
		// x: 0..320+220 plus 40px margins on both sides
		expect(d.width).toBe(620);
		expect(d.svg).toContain(`viewBox="0 0 ${d.width} ${d.height}"`);
	});

	test('escapes names, so a table name cannot break the XML', () => {
		const d = diagramSvg(ERD)!;
		expect(d.svg).toContain('a&lt;b&gt;&amp;&quot;c&quot;');
		expect(d.svg).not.toContain('a<b>');
		expect(esc(`<'&">`)).toBe('&lt;&#39;&amp;&quot;&gt;');
	});

	test('hidden tables and their refs are left out; nothing to draw → null', () => {
		const d = diagramSvg(ERD, { hidden: new Set(['users']) })!;
		expect(d.svg).not.toContain('>users<');
		expect(d.svg).not.toContain('marker-end');
		expect(diagramSvg(ERD, { hidden: new Set(['users', 'orders', 'odd']) })).toBeNull();
	});

	test('themes change the palette', () => {
		expect(diagramSvg(ERD, { theme: 'dark' })!.svg).toContain('fill="#0b0d10"');
		expect(diagramSvg(ERD, { theme: 'light' })!.svg).toContain('fill="#ffffff"');
	});
});

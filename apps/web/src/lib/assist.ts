// Suggestions for the text pane (DBML, SQL, Prisma). Pure: text and cursor in,
// options out, so it is unit-tested without CodeMirror (Editor.svelte adapts it).
// It reads the document being edited for table, column and enum names, and
// suggests only what the converter understands (no TableGroup, no checks).

import { scan, type Open } from './format';

export type Option = {
	label: string;
	type?: 'keyword' | 'type' | 'variable' | 'property' | 'constant' | 'function' | 'class';
	detail?: string;
	/** A CodeMirror snippet template (`${name}` marks a field the cursor visits). */
	snippet?: string;
	boost?: number;
};
export type Result = { from: number; options: Option[] };

const words = (labels: string[], type: Option['type'], detail?: string, boost?: number): Option[] =>
	labels.map((label) => ({ label, type, detail, boost }));

// ---------- types ----------
const DBML_TYPES = [
	'integer', 'int', 'bigint', 'smallint', 'serial', 'bigserial', 'varchar', 'char', 'text', 'boolean', 'bool',
	'timestamp', 'timestamptz', 'date', 'time', 'timetz', 'interval', 'uuid', 'json', 'jsonb', 'decimal',
	'numeric', 'float', 'double', 'real', 'money', 'bytea', 'inet'
];
const SQL_TYPES: Record<string, string[]> = {
	sql: [
		'integer', 'int', 'bigint', 'smallint', 'serial', 'bigserial', 'smallserial', 'varchar', 'char', 'text',
		'boolean', 'timestamp', 'timestamptz', 'date', 'time', 'interval', 'uuid', 'json', 'jsonb', 'numeric',
		'decimal', 'real', 'double precision', 'bytea', 'inet', 'money'
	],
	'sql:mysql': [
		'int', 'tinyint', 'smallint', 'mediumint', 'bigint', 'varchar', 'char', 'text', 'mediumtext', 'longtext',
		'datetime', 'timestamp', 'date', 'time', 'year', 'decimal', 'float', 'double', 'boolean', 'json', 'blob',
		'longblob', 'enum', 'bit'
	],
	'sql:sqlite': [
		'integer', 'int', 'bigint', 'text', 'varchar', 'real', 'float', 'double', 'numeric', 'blob', 'boolean',
		'datetime', 'date', 'timestamp'
	]
};
const PRISMA_TYPES = ['String', 'Int', 'BigInt', 'Float', 'Decimal', 'Boolean', 'DateTime', 'Json', 'Bytes'];

// ---------- what the document already declares ----------
export type Entities = { tables: Map<string, string[]>; enums: string[] };

const unquote = (s: string) => s.replace(/^["`]|["`]$/g, '');
const NAME = '("[^"]+"|`[^`]+`|[A-Za-z_][\\w.]*)';

/** Index just past the bracket matching the one at `open` (the end of `src` when unclosed). */
function blockEnd(src: string, open: number, o: string, c: string): number {
	let depth = 0;
	for (let i = open; i < src.length; i++) {
		if (src[i] === o) depth++;
		else if (src[i] === c && --depth === 0) return i;
	}
	return src.length;
}

// `text` without its comments: a `marker` comment runs to the end of the line, and block comments
// are dropped too. Quoted text is kept as it is.
function stripComments(text: string, marker: '--' | '//'): string {
	let out = '';
	let quote = '';
	for (let i = 0; i < text.length; i++) {
		const c = text[i];
		if (quote) {
			out += c;
			if (c === '\\' && quote !== "'") out += text[++i] ?? '';
			else if (c === quote) quote = '';
		} else if (c === "'" || c === '"' || c === '`') {
			quote = c;
			out += c;
		} else if (text.startsWith(marker, i)) {
			while (i < text.length && text[i] !== '\n') i++;
			i--; // keep the newline
		} else if (text.startsWith('/*', i)) {
			const end = text.indexOf('*/', i + 2);
			i = end < 0 ? text.length : end + 1;
			out += ' ';
		} else out += c;
	}
	return out;
}

/** First word of every top-level entry of a `{ }` or `( )` body, split on lines or commas. */
function firstWords(body: string, sep: '\n' | ',', skip: RegExp, marker: '--' | '//'): string[] {
	const out: string[] = [];
	let depth = 0;
	let piece = '';
	const flush = () => {
		const t = piece.trim();
		piece = '';
		if (!t || skip.test(t)) return;
		const m = new RegExp(`^${NAME}`).exec(t);
		if (m) out.push(unquote(m[1]));
	};
	let quote = ''; // brackets and separators inside a quoted string (DEFAULT 'a, b') are text
	for (const ch of stripComments(body, marker)) {
		if (quote) {
			if (ch === quote) quote = '';
		} else if (ch === "'" || ch === '"' || ch === '`') {
			quote = ch;
		} else if ('{(['.includes(ch)) depth++;
		else if ('})]'.includes(ch)) depth--;
		if (ch === sep && depth === 0 && !quote) flush();
		else piece += ch;
	}
	flush();
	return out;
}

export function entities(src: string, format: string): Entities {
	const tables = new Map<string, string[]>();
	const enums: string[] = [];
	if (format === 'dbml') {
		for (const m of src.matchAll(new RegExp(`\\bTable\\s+${NAME}(?:\\s+as\\s+\\w+)?\\s*\\{`, 'g'))) {
			const open = m.index + m[0].length - 1;
			const body = src.slice(open + 1, blockEnd(src, open, '{', '}'));
			tables.set(unquote(m[1]), firstWords(body, '\n', /^(indexes\s*\{|note\s*:)/i, '//'));
		}
		for (const m of src.matchAll(new RegExp(`\\bEnum\\s+${NAME}\\s*\\{`, 'g'))) enums.push(unquote(m[1]));
	} else if (format === 'prisma') {
		for (const m of src.matchAll(/\bmodel\s+(\w+)\s*\{/g)) {
			const open = m.index + m[0].length - 1;
			const body = src.slice(open + 1, blockEnd(src, open, '{', '}'));
			tables.set(m[1], firstWords(body, '\n', /^@/, '//'));
		}
		for (const m of src.matchAll(/\benum\s+(\w+)\s*\{/g)) enums.push(m[1]);
	} else {
		for (const m of src.matchAll(/\bCREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?((?:[\w"`]+\.)?[\w"`]+)\s*\(/gi)) {
			const open = m.index + m[0].length - 1;
			const body = src.slice(open + 1, blockEnd(src, open, '(', ')'));
			const name = unquote(m[1].split('.').pop()!);
			tables.set(name, firstWords(body, ',', /^(primary|foreign|unique|constraint|check|key|index|fulltext)\b/i, '--'));
		}
		for (const m of src.matchAll(/\bCREATE\s+TYPE\s+((?:[\w"]+\.)?[\w"]+)\s+AS\s+ENUM/gi)) enums.push(unquote(m[1].split('.').pop()!));
	}
	return { tables, enums };
}

const columnRefs = (e: Entities): Option[] =>
	[...e.tables].flatMap(([t, cols]) => cols.map((c) => ({ label: `${t}.${c}`, type: 'property' as const, detail: 'column' })));

// ---------- helpers on the cursor position ----------
const typeOptions = (types: string[], e: Entities): Option[] => [
	...words(e.enums, 'class', 'enum', 2),
	...words(types, 'type')
];

/** The innermost `{` around the cursor with the first word of its header (`Table`, `Indexes`, `model`, ...). */
function enclosingBlock(src: string, stack: Open[]): { kind: string; table?: string } | null {
	for (let i = stack.length - 1; i >= 0; i--) {
		if (stack[i].ch !== '{') continue;
		const lineStart = src.lastIndexOf('\n', stack[i].at) + 1;
		const header = src.slice(lineStart, stack[i].at).trim();
		const m = /^(\w+)(?:\s+("[^"]+"|`[^`]+`|[\w.]+))?/.exec(header);
		return m ? { kind: m[1].toLowerCase(), table: m[2] ? unquote(m[2]) : undefined } : { kind: '' };
	}
	return null;
}

/** The table a nested block (an `Indexes {}` inside `Table t {}`) belongs to. */
function owningTable(src: string, stack: Open[]): string | undefined {
	for (let i = stack.length - 1; i >= 0; i--) {
		if (stack[i].ch !== '{') continue;
		const lineStart = src.lastIndexOf('\n', stack[i].at) + 1;
		const m = new RegExp(`^\\s*(?:Table|model)\\s+${NAME}`).exec(src.slice(lineStart, stack[i].at));
		if (m) return unquote(m[1]);
	}
	return undefined;
}

/** Keywords at the start of a line are offered only while what is typed is a prefix of them. */
const starting = (word: string, options: Option[]): Option[] =>
	options.filter((o) => o.label.toLowerCase().startsWith(word.toLowerCase()));

const ACTIONS = ['cascade', 'restrict', 'set null', 'set default', 'no action'];
const SQL_ACTIONS = ['CASCADE', 'RESTRICT', 'SET NULL', 'SET DEFAULT', 'NO ACTION'];
const PRISMA_ACTIONS = ['Cascade', 'Restrict', 'NoAction', 'SetNull', 'SetDefault'];

// ---------- the entry point ----------
export function complete(format: string, src: string, pos: number, explicit = false): Result | null {
	const before = src.slice(0, pos);
	const lineStart = before.lastIndexOf('\n') + 1;
	const line = before.slice(lineStart);
	const word = /[@\w.]*$/.exec(line)![0];
	if (!word && !explicit) return null;

	const { lines, stack } = scan(before, format);
	const last = lines[lines.length - 1];
	if (last.endMode !== 'code' || last.lineComment) return null; // no suggestions inside a string or comment

	const ent = entities(src, format);
	const at = (n: number) => pos - n;
	const r = (from: number, options: Option[]): Result | null => (options.length ? { from, options } : null);
	const top = stack[stack.length - 1];
	const block = enclosingBlock(src, stack);

	if (format === 'dbml') return dbml(src, line, word, pos, top, block, stack, ent, r, at);
	if (format === 'prisma') return prisma(src, line, word, pos, top, block, stack, ent, r, at);
	return sql(format, line, word, top, ent, r, at);
}

type Reply = (from: number, options: Option[]) => Result | null;

function dbml(
	src: string, line: string, word: string, pos: number, top: Open | undefined,
	block: { kind: string } | null, stack: Open[], ent: Entities, r: Reply, at: (n: number) => number
): Result | null {
	// inside [ ... ]: column or index settings
	if (top?.ch === '[') {
		const inIndexes = block?.kind === 'indexes';
		const seg = src.slice(top.at + 1, pos);
		const piece = seg.slice(seg.lastIndexOf(',') + 1).trimStart();
		let m;
		if ((m = /^ref:\s*[<>-]\s*([\w."`]*)$/i.exec(piece))) return r(at(m[1].length), columnRefs(ent));
		if ((m = /^(?:delete|update):\s*([\w ]*)$/i.exec(piece))) return r(at(m[1].length), words(ACTIONS, 'constant'));
		if (!/^\w*$/.test(piece)) return null;
		const common = [
			{ label: 'pk', type: 'keyword' as const, detail: 'primary key' },
			{ label: 'unique', type: 'keyword' as const },
			{ label: 'note: ', type: 'property' as const, snippet: "note: '${}'" }
		];
		if (inIndexes)
			return r(at(piece.length), [
				...common,
				{ label: 'name: ', type: 'property', snippet: "name: '${}'" },
				{ label: 'type: ', type: 'property', snippet: 'type: ${btree}' }
			]);
		return r(at(piece.length), [
			...common,
			{ label: 'primary key', type: 'keyword' },
			{ label: 'not null', type: 'keyword' },
			{ label: 'null', type: 'keyword' },
			{ label: 'increment', type: 'keyword' },
			{ label: 'default: ', type: 'property', snippet: 'default: ${}' },
			{ label: 'ref: > ', type: 'property', detail: 'many-to-one' },
			{ label: 'ref: < ', type: 'property', detail: 'one-to-many' },
			{ label: 'ref: - ', type: 'property', detail: 'one-to-one' },
			{ label: 'delete: ', type: 'property', detail: 'cascade, set null, …' },
			{ label: 'update: ', type: 'property', detail: 'cascade, set null, …' }
		]);
	}
	// column names inside the parentheses of an index
	if (top?.ch === '(') {
		const table = owningTable(src, stack);
		return r(at(word.length), words(table ? (ent.tables.get(table) ?? []) : [], 'property', 'column'));
	}
	// a relationship: Ref: a.b > c.d  /  Ref name: ...  /  inside a Ref { } block
	if (/^\s*Ref\b/.test(line) && line.includes(':') || block?.kind === 'ref') {
		if (/\w/.test(word) || word === '') return r(at(word.length), columnRefs(ent));
	}
	if (block?.kind === 'table') {
		const m = /^\s*("[^"]*"|`[^`]*`|[A-Za-z_]\w*)\s+(\w*)$/.exec(line);
		if (m) return r(at(m[2].length), typeOptions(DBML_TYPES, ent));
		if (/^\s*\w*$/.test(line))
			return r(at(word.length), starting(word, [
				{ label: 'Indexes', type: 'keyword', snippet: 'Indexes {\n  (${columns}) [${unique}]\n}' },
				{ label: 'Note: ', type: 'keyword', snippet: "Note: '${}'" }
			]));
		return null;
	}
	if (!block && /^\s*\w*$/.test(line))
		return r(at(word.length), starting(word, [
			{ label: 'Table', type: 'keyword', snippet: 'Table ${name} {\n  id integer [pk]\n  ${}\n}' },
			{ label: 'Ref: ', type: 'keyword', snippet: 'Ref: ${a.id} > ${b.a_id}' },
			{ label: 'Enum', type: 'keyword', snippet: 'Enum ${name} {\n  ${value}\n}' },
			{ label: 'Project', type: 'keyword', snippet: "Project ${name} {\n  database_type: '${PostgreSQL}'\n}" }
		]));
	return null;
}

function sql(
	format: string, line: string, word: string, top: Open | undefined, ent: Entities, r: Reply, at: (n: number) => number
): Result | null {
	const types = SQL_TYPES[format] ?? SQL_TYPES.sql;
	let m;
	if ((m = /\breferences\s+((?:[\w"`]+\.)?[\w"`]*)$/i.exec(line))) return r(at(m[1].length), words([...ent.tables.keys()], 'class', 'table'));
	if ((m = /\breferences\s+((?:[\w"`]+\.)?[\w"`]+)\s*\(\s*(?:[\w"`]+\s*,\s*)*([\w"`]*)$/i.exec(line)))
		return r(at(m[2].length), words(ent.tables.get(unquote(m[1].split('.').pop()!)) ?? [], 'property', 'column'));
	if ((m = /\bon\s+(?:delete|update)\s+([\w ]*)$/i.exec(line))) return r(at(m[1].length), words(SQL_ACTIONS, 'constant'));
	if (/^\s*create\s+(?:unique\s+)?index\b/i.test(line) && (m = /\bon\s+([\w"`.]*)$/i.exec(line)))
		return r(at(m[1].length), words([...ent.tables.keys()], 'class', 'table'));
	if (top?.ch === '(') {
		const col = /^\s*("[^"]+"|`[^`]+`|[A-Za-z_]\w*)\s+(\w*)$/.exec(line);
		if (col && !/^(primary|foreign|unique|constraint|check|key|index|fulltext)$/i.test(col[1]))
			return r(at(col[2].length), typeOptions(types, ent));
		if (/^\s*\S+\s+\S/.test(line) && /\s\w*$/.test(line))
			return r(at(word.length), words(
				['PRIMARY KEY', 'NOT NULL', 'NULL', 'UNIQUE', 'DEFAULT', 'REFERENCES', ...(format === 'sql:mysql' ? ['AUTO_INCREMENT'] : [])],
				'keyword'
			));
		return r(at(word.length), words(['PRIMARY KEY', 'FOREIGN KEY', 'UNIQUE', 'CONSTRAINT', 'CHECK'], 'keyword'));
	}
	if (/^\s*\w*$/.test(line))
		return r(at(word.length), starting(word, [
			{ label: 'CREATE TABLE', type: 'keyword', snippet: 'CREATE TABLE ${name} (\n  id integer PRIMARY KEY,\n  ${}\n);' },
			{ label: 'CREATE INDEX', type: 'keyword', snippet: 'CREATE INDEX ${name} ON ${table} (${column});' },
			{ label: 'CREATE UNIQUE INDEX', type: 'keyword', snippet: 'CREATE UNIQUE INDEX ${name} ON ${table} (${column});' },
			...(format === 'sql' ? [{ label: 'CREATE TYPE', type: 'keyword' as const, snippet: "CREATE TYPE ${name} AS ENUM ('${a}', '${b}');" }] : [])
		]));
	return null;
}

function prisma(
	src: string, line: string, word: string, pos: number, top: Open | undefined,
	block: { kind: string } | null, stack: Open[], ent: Entities, r: Reply, at: (n: number) => number
): Result | null {
	let m;
	if (top?.ch === '(') {
		const head = src.slice(Math.max(0, top.at - 12), top.at);
		const inside = src.slice(top.at + 1, pos);
		if (/@default$/.test(head))
			return r(at(word.length), [
				...words(['autoincrement()', 'now()', 'uuid()', 'cuid()', 'dbgenerated("")', 'auto()'], 'function'),
				...words(['true', 'false'], 'constant'),
				...words(ent.enums, 'class', 'enum')
			]);
		if (/@relation$/.test(head)) {
			if ((m = /(?:onDelete|onUpdate):\s*(\w*)$/.exec(inside))) return r(at(m[1].length), words(PRISMA_ACTIONS, 'constant'));
			return r(at(word.length), [
				{ label: 'fields: ', type: 'property', snippet: 'fields: [${}]' },
				{ label: 'references: ', type: 'property', snippet: 'references: [${}]' },
				{ label: 'onDelete: ', type: 'property' },
				{ label: 'onUpdate: ', type: 'property' },
				{ label: 'name: ', type: 'property', snippet: 'name: "${}"' }
			]);
		}
		return null;
	}
	if (top?.ch === '[') {
		const table = owningTable(src, stack);
		return r(at(word.length), words(table ? (ent.tables.get(table) ?? []) : [], 'property', 'field'));
	}
	if (block?.kind === 'model') {
		if (word.startsWith('@@'))
			return r(at(word.length), [
				{ label: '@@id', type: 'keyword', snippet: '@@id([${}])' },
				{ label: '@@unique', type: 'keyword', snippet: '@@unique([${}])' },
				{ label: '@@index', type: 'keyword', snippet: '@@index([${}])' },
				{ label: '@@map', type: 'keyword', snippet: '@@map("${}")' }
			]);
		if (word.startsWith('@'))
			return r(at(word.length), [
				{ label: '@id', type: 'keyword' },
				{ label: '@unique', type: 'keyword' },
				{ label: '@default', type: 'keyword', snippet: '@default(${})' },
				{ label: '@relation', type: 'keyword', snippet: '@relation(fields: [${}], references: [${}])' },
				{ label: '@updatedAt', type: 'keyword' },
				{ label: '@map', type: 'keyword', snippet: '@map("${}")' }
			]);
		if ((m = /^\s*\w+\s+(\w*)$/.exec(line)))
			return r(at(m[1].length), [...words([...ent.tables.keys()], 'class', 'model', 2), ...typeOptions(PRISMA_TYPES, ent)]);
		return null;
	}
	if (!block && /^\s*\w*$/.test(line))
		return r(at(word.length), starting(word, [
			{ label: 'model', type: 'keyword', snippet: 'model ${Name} {\n  id Int @id @default(autoincrement())\n  ${}\n}' },
			{ label: 'enum', type: 'keyword', snippet: 'enum ${Name} {\n  ${VALUE}\n}' },
			{ label: 'datasource', type: 'keyword', snippet: 'datasource db {\n  provider = "${postgresql}"\n  url      = env("DATABASE_URL")\n}' },
			{ label: 'generator', type: 'keyword', snippet: 'generator client {\n  provider = "prisma-client-js"\n}' }
		]));
	return null;
}

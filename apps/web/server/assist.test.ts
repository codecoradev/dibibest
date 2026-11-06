import { expect, test } from 'bun:test';
import { complete, entities } from '../src/lib/assist';

/** Suggestions at the `|` in `doc`. */
function at(format: string, doc: string, explicit = false) {
	const pos = doc.indexOf('|');
	const src = doc.replace('|', '');
	const r = complete(format, src, pos, explicit);
	return r ? { from: r.from, text: src.slice(r.from, pos), labels: r.options.map((o) => o.label) } : null;
}

test('a column type: int offers int and integer; the enums of the document come first', () => {
	const r = at('dbml', 'Enum status {\n  a\n}\nTable t {\n  id int|\n}');
	expect(r!.text).toBe('int');
	expect(r!.labels).toContain('integer');
	expect(r!.labels).toContain('int');
	expect(r!.labels[0]).toBe('status');
	// only in the type slot, not while naming the column
	expect(at('dbml', 'Table t {\n  id|\n}')).toBeNull();
});

test('nothing is offered inside a comment or a string', () => {
	expect(at('dbml', 'Table t {\n  // id int|\n}')).toBeNull();
	expect(at('dbml', "Table t {\n  id text [note: 'int|']\n}")).toBeNull();
});

test('column settings, and actions after delete:', () => {
	const s = at('dbml', 'Table t {\n  id int [pk, no|]\n}')!;
	expect(s.labels).toContain('not null');
	const a = at('dbml', 'Table t {\n  a int [delete: ca|]\n}')!;
	expect(a.text).toBe('ca');
	expect(a.labels).toContain('cascade');
});

test('relationships suggest table.column from the document', () => {
	const doc = 'Table users {\n  id int\n  email text\n}\nTable posts {\n  user_id int\n}\nRef: posts.user_id > us|';
	const r = at('dbml', doc)!;
	expect(r.text).toBe('us');
	expect(r.labels).toContain('users.id');
	expect(r.labels).toContain('posts.user_id');
	const inline = at('dbml', doc.replace('Ref: posts.user_id > us|', '') + 'Table c {\n  uid int [ref: > users.|]\n}')!;
	expect(inline.labels).toContain('users.email');
});

test('top-level keywords, only those the converter understands', () => {
	const r = at('dbml', 'T|')!;
	expect(r.labels).toContain('Table');
	expect(r.labels).not.toContain('TableGroup');
});

test('index columns come from the enclosing table', () => {
	const r = at('dbml', 'Table t {\n  id int\n  email text\n  Indexes {\n    (em|)\n  }\n}')!;
	expect(r.labels).toEqual(['id', 'email']);
});

test('SQL: types per dialect, tables after REFERENCES, columns inside its parentheses', () => {
	expect(at('sql', 'CREATE TABLE t (\n  id ser|\n);')!.labels).toContain('serial');
	expect(at('sql:mysql', 'CREATE TABLE t (\n  id lon|\n);')!.labels).toContain('longtext');
	expect(at('sql:mysql', 'CREATE TABLE t (\n  id lon|\n);')!.labels).not.toContain('serial');
	const doc = 'CREATE TABLE users (\n  id int,\n  email text\n);\nCREATE TABLE posts (\n  uid int REFERENCES us|\n);';
	expect(at('sql', doc)!.labels).toContain('users');
	const cols = at('sql', 'CREATE TABLE users (\n  id int,\n  email text\n);\nCREATE TABLE p (\n  uid int REFERENCES users(e|)\n);')!;
	expect(cols.labels).toEqual(['id', 'email']);
	expect(at('sql', 'CREATE TABLE p (\n  uid int REFERENCES users(id) ON DELETE ca|\n);')!.labels).toContain('CASCADE');
});

test('comments never hide a column or invent one', () => {
	// SQL: a comment before a column shares its comma-separated piece; a comma inside a comment splits it
	const sql = entities(
		"CREATE TABLE t (\n  -- the key\n  id int, -- first, primary\n  /* a, b */\n  name text,\n  note text DEFAULT '--x, y'\n);",
		'sql'
	);
	expect(sql.tables.get('t')).toEqual(['id', 'name', 'note']);
	const dbml = entities('Table t {\n  // old: gone\n  id int // the id\n  /* x\n y */\n  name text\n}', 'dbml');
	expect(dbml.tables.get('t')).toEqual(['id', 'name']);
	const prisma = entities('model M {\n  // c\n  id Int @id // primary, key\n  name String\n}', 'prisma');
	expect(prisma.tables.get('M')).toEqual(['id', 'name']);
});

test('Prisma: field types include the models and enums, attributes, defaults', () => {
	const doc = 'enum Role {\n  A\n}\nmodel User {\n  id Int @id\n}\nmodel Post {\n  author Us|\n}';
	const t = at('prisma', doc)!;
	expect(t.labels).toContain('User');
	expect(t.labels).toContain('Role');
	expect(t.labels).toContain('DateTime');
	expect(at('prisma', 'model A {\n  id Int @|\n}')!.labels).toContain('@default');
	expect(at('prisma', 'model A {\n  id Int @default(au|)\n}')!.labels).toContain('autoincrement()');
	expect(at('prisma', 'model A {\n  id Int\n  @@in|\n}')!.labels).toEqual(['@@id', '@@unique', '@@index', '@@map']);
});

test('entities: tables, columns and enums of each format', () => {
	const d = entities('Table "a b" as X {\n  id int\n  Indexes {\n    (id)\n  }\n  note text\n}\nEnum e {\n  x\n}', 'dbml');
	expect([...d.tables]).toEqual([['a b', ['id', 'note']]]);
	expect(d.enums).toEqual(['e']);
	const s = entities('CREATE TYPE mood AS ENUM (\'a\');\nCREATE TABLE public.t (\n  id int,\n  PRIMARY KEY (id),\n  name text\n);', 'sql');
	expect([...s.tables]).toEqual([['t', ['id', 'name']]]);
	expect(s.enums).toEqual(['mood']);
});

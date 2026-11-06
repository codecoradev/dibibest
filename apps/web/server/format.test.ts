import { expect, test } from 'bun:test';
import { formatSource } from '../src/lib/format';

const f = (s: string, fmt = 'dbml') => formatSource(s, fmt);

test('indents by bracket depth, steps back out on a closer', () => {
	expect(f('Table t {\nid int\n  name varchar [pk]\n   }\n')).toBe('Table t {\n  id int\n  name varchar [pk]\n}\n');
	expect(f('Table t {\nIndexes {\n(a, b) [unique]\n}\n}\n')).toBe('Table t {\n  Indexes {\n    (a, b) [unique]\n  }\n}\n');
});

test('trailing spaces go, one blank line at most, none at the top, exactly one final newline', () => {
	expect(f('\n\nTable a {   \n  id int  \n}\n\n\n\nTable b {\n}\n\n\n')).toBe('Table a {\n  id int\n}\n\nTable b {\n}\n');
	expect(f('Table a {\n  id int\n}')).toBe('Table a {\n  id int\n}\n');
	expect(f('Table a {\n  id int\n}\n   ')).toBe('Table a {\n  id int\n}\n');
	expect(f('a\n  \n  \nb\n')).toBe('a\n\nb\n');
});

test('brackets inside strings and comments do not count; their text is left alone', () => {
	const src = "Table t {\n  note1 text [note: 'a { b']\n  // a comment with }\n  /* block {\n     kept as typed   \n  */\n  id int\n}\n";
	expect(f(src)).toBe(src);
	// SQL: -- comments and a multi-line string literal
	const sql = "CREATE TABLE t (\n  a text default 'one\n   two  ',\n  b int -- )\n);\n";
	expect(f(sql, 'sql')).toBe(sql);
});

test('SQL parentheses and Prisma blocks', () => {
	expect(f('CREATE TABLE a (\nid int,\nPRIMARY KEY (id)\n);\n', 'sql')).toBe('CREATE TABLE a (\n  id int,\n  PRIMARY KEY (id)\n);\n');
	expect(f('model A {\nid Int @id\n@@index([id])\n}\n', 'prisma')).toBe('model A {\n  id Int @id\n  @@index([id])\n}\n');
});

test('idempotent, and only whitespace ever changes', () => {
	const messy = '  Table a {\nid int [pk]   \n\n\n   name text [note: \'x\']\n}\nRef: a.id > b.a_id\n\nTable b {\n a_id int\n }';
	const once = f(messy);
	expect(f(once)).toBe(once);
	expect(once.replace(/\s+/g, '')).toBe(messy.replace(/\s+/g, ''));
});

test('empty and blank input', () => {
	expect(f('')).toBe('');
	expect(f('  \n \n')).toBe('');
});

test('an unclosed string does not swallow the rest of the document', () => {
	// a stray apostrophe ends with its line in DBML
	expect(f("Table t {\nid int [note: 'oops]\n   name text\n}\n")).toBe("Table t {\n  id int [note: 'oops]\n  name text\n}\n");
});

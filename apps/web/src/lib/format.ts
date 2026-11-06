// Whitespace-only formatter and the scanner it shares with the completion
// source (assist.ts). It never changes a token, only the indentation, trailing
// spaces and blank lines, so the schema means exactly what it did before: unlike
// a parse-and-emit round trip it keeps comments and the order and layout of the
// source. Text inside strings and block comments is left alone.

export type Edit = { from: number; to: number; insert: string };

const OPEN = '{([';
const CLOSE = '})]';
const UNIT = '  ';

type Mode = 'code' | 'sq' | 'dq' | 'bt' | 'tq' | 'block';

export type LineInfo = {
	/** Offset of the first character of the line. */
	start: number;
	text: string;
	/** The line begins inside a multi-line string or block comment. */
	insideStart: boolean;
	/** The line ends inside one (a single-line string ends with its line). */
	insideEnd: boolean;
	/** Indentation depth at the start of the line: the `{` and `(` still open (a `[` does not indent). */
	depthStart: number;
	/** How many of the line's leading closers (`}` `)`) end a block that is open: they step the line back out. */
	leadClosers: number;
	/** Scanner mode at the end of the line, before a single-line string is closed. */
	endMode: Mode;
	/** A `//` or `--` comment runs to the end of the line. */
	lineComment: boolean;
};

export type Open = { ch: string; at: number };

const isSql = (format: string) => format.startsWith('sql');
const indents = (o: Open) => o.ch === '{' || o.ch === '(';
const OPENER_OF: Record<string, string> = { '}': '{', ')': '(', ']': '[' };

/**
 * Walk `src` once, line by line, tracking strings, comments and brackets.
 * `stack` is the brackets still open at the end of the text.
 */
export function scan(src: string, format: string): { lines: LineInfo[]; stack: Open[] } {
	const sql = isSql(format);
	const out: LineInfo[] = [];
	const stack: Open[] = [];
	let mode: Mode = 'code';
	let start = 0;
	for (const text of src.split('\n')) {
		const info: LineInfo = {
			start,
			text,
			insideStart: mode !== 'code',
			insideEnd: false,
			depthStart: stack.filter(indents).length,
			leadClosers: 0,
			endMode: 'code',
			lineComment: false
		};
		let i = 0;
		let leading = true;
		while (i < text.length) {
			const c = text[i];
			if (mode === 'code') {
				if (c !== ' ' && c !== '\t' && !CLOSE.includes(c)) leading = false;
				if (text.startsWith(sql ? '--' : '//', i)) {
					info.lineComment = true;
					break;
				}
				if (text.startsWith('/*', i)) {
					mode = 'block';
					i += 2;
					continue;
				}
				if (!sql && text.startsWith("'''", i)) {
					mode = 'tq';
					i += 3;
					continue;
				}
				if (c === "'") mode = 'sq';
				else if (c === '"') mode = 'dq';
				else if (c === '`') mode = 'bt';
				else if (OPEN.includes(c)) stack.push({ ch: c, at: start + i });
				else if (CLOSE.includes(c)) {
					// close the nearest matching opener; a stale one above it (a `[` left open by a typo) goes too
					const at = stack.map((o) => o.ch).lastIndexOf(OPENER_OF[c]);
					if (at >= 0) {
						const closed = stack.splice(at).filter(indents).length;
						if (leading) info.leadClosers += closed;
					}
				}
				i++;
			} else if (mode === 'block') {
				if (text.startsWith('*/', i)) {
					mode = 'code';
					i += 2;
				} else i++;
			} else if (mode === 'tq') {
				if (text.startsWith("'''", i)) {
					mode = 'code';
					i += 3;
				} else i++;
			} else if (c === '\\' && !(sql && mode === 'sq')) {
				i += 2;
			} else {
				if ((mode === 'sq' && c === "'") || (mode === 'dq' && c === '"') || (mode === 'bt' && c === '`')) mode = 'code';
				i++;
			}
		}
		info.endMode = mode;
		// a quoted string ends with its line; only an SQL string literal may span lines
		if (mode === 'dq' || mode === 'bt' || (mode === 'sq' && !sql)) mode = 'code';
		info.insideEnd = mode !== 'code';
		out.push(info);
		start += text.length + 1;
	}
	return { lines: out, stack };
}

/** The changes that tidy `src`, as offsets into `src` (what a CodeMirror dispatch takes). */
export function formatEdits(src: string, format: string): Edit[] {
	if (!src.trim()) return src ? [{ from: 0, to: src.length, insert: '' }] : [];
	const { lines } = scan(src, format);
	const endsWithNewline = src.endsWith('\n');
	// the piece after the final newline is not a line
	const n = endsWithNewline ? lines.length - 1 : lines.length;
	const isBlank = (i: number) => !lines[i].insideStart && lines[i].text.trim() === '';
	let lastContent = -1;
	for (let i = 0; i < n; i++) if (!isBlank(i)) lastContent = i;

	const edits: Edit[] = [];
	let prevBlank = true; // blank lines at the top of the text are dropped
	let lastDeleted = false;
	for (let i = 0; i < n; i++) {
		const l = lines[i];
		const newlineAfter = i < lines.length - 1;
		if (isBlank(i)) {
			if (i > lastContent || prevBlank) {
				// a blank line at the end, or a second one in a row: delete the line with its newline
				edits.push({ from: l.start, to: l.start + l.text.length + (newlineAfter ? 1 : 0), insert: '' });
				if (i === n - 1) lastDeleted = true;
			} else if (l.text !== '') {
				edits.push({ from: l.start, to: l.start + l.text.length, insert: '' });
			}
			prevBlank = true;
			continue;
		}
		prevBlank = false;
		if (l.insideStart) continue; // the inside of a multi-line string or comment is not ours
		const lead = l.text.length - l.text.trimStart().length;
		const want = UNIT.repeat(Math.max(0, l.depthStart - l.leadClosers));
		if (l.text.slice(0, lead) !== want) edits.push({ from: l.start, to: l.start + lead, insert: want });
		if (!l.insideEnd) {
			const end = l.text.trimEnd().length;
			if (end < l.text.length) edits.push({ from: l.start + end, to: l.start + l.text.length, insert: '' });
		}
	}
	// exactly one newline at the end (unless the last blank line was removed and the line before it already has one)
	if (!endsWithNewline && !lastDeleted) edits.push({ from: src.length, to: src.length, insert: '\n' });
	return edits;
}

export function applyEdits(src: string, edits: Edit[]): string {
	let out = src;
	for (const e of [...edits].sort((a, b) => b.from - a.from || b.to - a.to)) out = out.slice(0, e.from) + e.insert + out.slice(e.to);
	return out;
}

export function formatSource(src: string, format: string): string {
	const text = src.replace(/\r\n?/g, '\n');
	return applyEdits(text, formatEdits(text, format));
}

<script lang="ts">
	import { untrack } from 'svelte';
	import { EditorState } from '@codemirror/state';
	import { EditorView, keymap, lineNumbers } from '@codemirror/view';
	import { defaultKeymap, history, historyKeymap, indentMore, indentLess } from '@codemirror/commands';
	import {
		HighlightStyle,
		StreamLanguage,
		bracketMatching,
		indentOnInput,
		indentUnit,
		syntaxHighlighting
	} from '@codemirror/language';
	import {
		acceptCompletion,
		autocompletion,
		closeBrackets,
		closeBracketsKeymap,
		closeCompletion,
		moveCompletionSelection,
		snippetCompletion,
		startCompletion,
		type Completion,
		type CompletionSource
	} from '@codemirror/autocomplete';
	import { tags as t } from '@lezer/highlight';
	import { complete, type Option } from '../assist';
	import { formatEdits } from '../format';

	let { value, readonly = false, label, format = 'dbml', onchange }: {
		value: string;
		readonly?: boolean;
		label: string;
		/** Source format id (dbml, sql, sql:mysql, sql:sqlite, prisma): what to suggest and how to indent. */
		format?: string;
		onchange?: (v: string) => void;
	} = $props();

	// Deliberately tiny shared tokenizer for DBML / SQL / Prisma — keywords, strings, comments, [settings].
	const KEYWORDS =
		/^(Table|Ref|Enum|Project|Indexes|Note|TableGroup|create|table|primary|key|foreign|references|not|null|unique|default|index|on|constraint|model|datasource|generator|enum|alter|add|if|exists)\b/i;
	const lang = StreamLanguage.define<{ inSettings: boolean; inBlock: boolean; depth: number }>({
		startState: () => ({ inSettings: false, inBlock: false, depth: 0 }),
		// a line is indented by the brackets open above it; a leading closer steps back out
		indent: (state, textAfter, cx) => Math.max(0, state.depth - (/^\s*[})]/.test(textAfter) ? 1 : 0)) * cx.unit,
		languageData: {
			indentOnInput: /^\s*[})]$/,
			closeBrackets: { brackets: ['(', '[', '{', "'", '"', '`'] }
		},
		token(stream, state) {
			if (state.inBlock) {
				if (stream.skipTo('*/')) {
					stream.match('*/');
					state.inBlock = false;
				} else stream.skipToEnd();
				return 'comment';
			}
			if (stream.eatSpace()) return null;
			if (stream.match('/*')) {
				state.inBlock = true;
				return 'comment';
			}
			if (stream.match(/^(\/\/|--).*/)) return 'comment';
			if (stream.match(/^('[^']*'|"[^"]*"|`[^`]*`)/)) return 'string';
			if (stream.eat('[')) return ((state.inSettings = true), 'bracket');
			if (stream.eat(']')) return ((state.inSettings = false), 'bracket');
			if (stream.match(KEYWORDS)) return 'keyword';
			if (stream.match(/^[0-9][0-9.]*/)) return 'number';
			if (state.inSettings && stream.match(/^[a-z_]+/i)) return 'atom';
			const ch = stream.next();
			if (ch === '{' || ch === '(') state.depth++;
			else if ((ch === '}' || ch === ')') && state.depth > 0) state.depth--;
			return null;
		}
	});
	const highlight = HighlightStyle.define([
		{ tag: t.keyword, color: '#7da2ff' },
		{ tag: t.comment, color: '#8b95a5', fontStyle: 'italic' },
		{ tag: t.string, color: '#e8c07d' },
		{ tag: t.number, color: '#e8c07d' },
		{ tag: t.atom, color: '#b48cff' }
	]);
	const theme = EditorView.theme(
		{
			'&': { height: '100%', color: 'var(--text-1)', backgroundColor: 'transparent', fontSize: '13px' },
			'.cm-scroller': { fontFamily: 'var(--mono)', lineHeight: '1.7' },
			'.cm-gutters': { backgroundColor: 'transparent', color: 'var(--text-3)', border: 'none' },
			'.cm-activeLine, .cm-activeLineGutter': { backgroundColor: 'var(--accent-soft)' },
			'.cm-cursor': { borderLeftColor: 'var(--text-1)' },
			'&.cm-focused': { outline: 'none' },
			'.cm-tooltip': { backgroundColor: 'var(--bg-2)', border: '1px solid var(--line)', borderRadius: 'var(--r-md)', overflow: 'hidden' },
			'.cm-tooltip.cm-tooltip-autocomplete > ul': { fontFamily: 'var(--mono)', fontSize: '12.5px', maxHeight: '15em' },
			'.cm-tooltip-autocomplete ul li': { padding: '3px 10px', color: 'var(--text-2)' },
			'.cm-tooltip-autocomplete ul li[aria-selected]': { backgroundColor: 'var(--accent-strong)', color: '#fff' },
			'.cm-completionDetail': { marginLeft: '1.2em', fontStyle: 'normal', opacity: '0.7' },
			'.cm-completionMatchedText': { textDecoration: 'none', fontWeight: '700', color: 'var(--text-1)' },
			'.cm-matchingBracket': { backgroundColor: 'var(--accent-soft)', outline: '1px solid var(--accent)' }
		},
		{ dark: true }
	);

	let host: HTMLDivElement;
	let view: EditorView | undefined;

	const toCompletion = (o: Option): Completion =>
		o.snippet
			? snippetCompletion(o.snippet, { label: o.label, type: o.type, detail: o.detail, boost: o.boost })
			: { label: o.label, type: o.type, detail: o.detail, boost: o.boost };
	// `format` is read when a suggestion is asked for, so switching the pane's format needs no rebuild
	const source: CompletionSource = (ctx) => {
		const r = complete(format, ctx.state.doc.toString(), ctx.pos, ctx.explicit);
		return r ? { from: r.from, options: r.options.map(toCompletion), validFor: /^[@\w.]*$/ } : null;
	};

	// Tab indents and accepts a suggestion. To keep keyboard users from being trapped in
	// the editor, Escape then Tab moves focus on to the next control instead.
	let escaped = false;
	const assistKeys = [
		...closeBracketsKeymap,
		{ key: 'ArrowDown', run: moveCompletionSelection(true) },
		{ key: 'ArrowUp', run: moveCompletionSelection(false) },
		{ key: 'Ctrl-Space', run: startCompletion },
		{
			key: 'Escape',
			run: (v: EditorView) => {
				if (closeCompletion(v)) return true;
				escaped = true;
				return false;
			}
		},
		{
			key: 'Tab',
			run: (v: EditorView) => {
				if (escaped) {
					escaped = false;
					return false;
				}
				return acceptCompletion(v) || indentMore(v);
			},
			shift: (v: EditorView) => {
				if (escaped) {
					escaped = false;
					return false;
				}
				return indentLess(v);
			}
		},
		{ key: 'Shift-Alt-f', run: () => (beautify(), true) }
	];

	/** Tidy the indentation, trailing spaces and blank lines. Only whitespace changes. Returns whether anything did. */
	export function beautify(): boolean {
		if (!view || readonly) return false;
		const edits = formatEdits(view.state.doc.toString(), format);
		if (!edits.length) return false;
		view.dispatch({ changes: edits.map((e) => ({ from: e.from, to: e.to, insert: e.insert })), userEvent: 'format' });
		return true;
	}

	// Create once; props are read untracked so a value change never rebuilds the view.
	$effect(() => {
		view = untrack(() => new EditorView({
			parent: host,
			state: EditorState.create({
				doc: value,
				extensions: [
					lineNumbers(),
					history(),
					...(readonly
						? []
						: [
								closeBrackets(),
								autocompletion({ override: [source], defaultKeymap: false, icons: false, maxRenderedOptions: 40, interactionDelay: 0 }),
								indentOnInput(),
								bracketMatching(),
								indentUnit.of('  '),
								EditorState.tabSize.of(2),
								keymap.of(assistKeys),
								EditorView.domEventHandlers({
									keydown: (e) => {
										if (e.key !== 'Escape' && e.key !== 'Tab') escaped = false;
										return false;
									}
								})
							]),
					keymap.of([...defaultKeymap, ...historyKeymap]),
					lang,
					syntaxHighlighting(highlight),
					theme,
					EditorState.readOnly.of(readonly),
					EditorView.editable.of(!readonly),
					EditorView.contentAttributes.of({ 'aria-label': label }),
					EditorView.updateListener.of((u) => u.docChanged && u.state.doc.toString() !== value && onchange?.(u.state.doc.toString()))
				]
			})
		}));
		return () => view?.destroy();
	});

	// Store → editor (canvas edits, format switches). Skip when the editor already shows it.
	$effect(() => {
		if (view && value !== view.state.doc.toString()) {
			view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value } });
		}
	});
</script>

<div class="editor" bind:this={host}></div>

<style>
	.editor {
		flex: 1;
		min-height: 0;
		padding: var(--sp-3) 0;
		overflow: hidden;
	}
</style>

// The one workspace store. Every layout reads `erd` (ERD-JSON, $state.raw) — there
// is no second model. Text and canvas edits both end in `analyze`, so what you see
// is always what the core parsed.
import { convert, toErd, type ErdDoc, type ErdNode, type Fidelity } from './core';
import { diagramSvg, svgToPng, type Theme } from './diagram';
import {
	deleteProject,
	download,
	extensionFor,
	fileBase,
	formatFromFileName,
	getProject,
	lastProjectId,
	listProjects,
	newId,
	persistent,
	saveProject,
	type ProjectSummary
} from './projects';

export type Layout = 'text' | 'canvas' | 'explorer';
export const FORMATS = [
	{ id: 'dbml', label: 'DBML' },
	{ id: 'sql', label: 'SQL' },
	{ id: 'prisma', label: 'Prisma' }
] as const;

export const SAMPLE = `Table users {
  id integer [pk]
  email varchar [not null, unique]
  created_at datetime
}

Table orders {
  id integer [pk]
  user_id integer [not null]
  status varchar
  total_cents integer [default: 0]
}

Ref: orders.user_id > users.id
`;

const DEBOUNCE_MS = 150;
const SAVE_MS = 400;
const BLANK = `Table new_table {
  id integer [pk]
}
`;

export type SaveState = 'saved' | 'saving' | 'unsaved' | 'error';

class Workspace {
	layout = $state<Layout>('text');
	format = $state('dbml');
	text = $state(SAMPLE);
	erd = $state.raw<ErdDoc | null>(null);
	fidelity = $state.raw<Fidelity | null>(null);
	error = $state<string | null>(null);
	parseMs = $state(0);
	/** Explorer: ids of tables hidden by the sidebar checkboxes. */
	hidden = $state.raw<ReadonlySet<string>>(new Set());
	/** Explorer focus mode: id of the focused table. */
	focus = $state<string | null>(null);

	/** Local project (IndexedDB). */
	projectId = $state<string | null>(null);
	projectName = $state('shop-demo');
	projects = $state.raw<ProjectSummary[]>([]);
	saveState = $state<SaveState>('saved');
	/** False when IndexedDB is unavailable: projects last for this session only. */
	persistent = $state(true);

	#timer: ReturnType<typeof setTimeout> | undefined;
	#saveTimer: ReturnType<typeof setTimeout> | undefined;
	/** Bumped on every change; a save only reports "saved" for the latest. */
	#rev = 0;
	#seq = 0;

	/** Open the last project, or the newest, or create the sample project. */
	async boot() {
		this.persistent = await persistent();
		await this.refreshProjects();
		const id = lastProjectId();
		const target = this.projects.find((p) => p.id === id) ?? this.projects[0];
		if (target) return this.openProject(target.id);
		this.projectId = newId();
		this.projectName = 'shop-demo';
		await this.analyze();
		await this.saveNow();
	}

	async refreshProjects() {
		this.projects = await listProjects().catch(() => []);
	}

	/** Debounced autosave after any change to the text or the canvas. */
	scheduleSave() {
		if (!this.projectId) return;
		this.#rev++;
		this.saveState = 'unsaved';
		clearTimeout(this.#saveTimer);
		this.#saveTimer = setTimeout(() => this.saveNow(), SAVE_MS);
	}

	async saveNow() {
		clearTimeout(this.#saveTimer);
		if (!this.projectId) return;
		// Snapshot BEFORE the await: the user may switch, rename or delete the
		// project, or keep typing, while IndexedDB writes.
		const snap = {
			id: this.projectId,
			name: this.projectName,
			format: this.format,
			text: this.text,
			erd: this.erd,
			updatedAt: Date.now()
		};
		const rev = this.#rev;
		this.saveState = 'saving';
		try {
			await saveProject(snap);
		} catch {
			if (snap.id === this.projectId) this.saveState = 'error';
			return;
		}
		if (snap.id === this.projectId) {
			// newer edits arrived during the write: their own save is scheduled
			this.saveState = rev === this.#rev ? 'saved' : 'unsaved';
		}
		// Update the list in memory (re-reading every project body on each autosave
		// would be wasteful). Never resurrect a project deleted meanwhile.
		if (snap.id === this.projectId || this.projects.some((p) => p.id === snap.id)) {
			const summary = { id: snap.id, name: snap.name, format: snap.format, updatedAt: snap.updatedAt };
			this.projects = [summary, ...this.projects.filter((p) => p.id !== snap.id)];
		}
	}

	async openProject(id: string) {
		if (this.saveState === 'unsaved') await this.saveNow();
		const p = await getProject(id);
		if (!p) return;
		this.projectId = p.id;
		this.projectName = p.name;
		this.format = p.format;
		this.text = p.text;
		this.erd = p.erd; // positions survive: analyze keeps them by node id
		this.hidden = new Set();
		this.focus = null;
		await this.analyze(false);
		this.saveState = 'saved';
		await this.saveNow(); // remembers it as the last project
	}

	async newProject(name = 'Untitled', format = 'dbml', text = BLANK, erd: ErdDoc | null = null) {
		if (this.saveState === 'unsaved') await this.saveNow();
		this.projectId = newId();
		this.projectName = name;
		this.format = format;
		this.text = text;
		this.erd = erd;
		this.hidden = new Set();
		this.focus = null;
		await this.analyze(false);
		await this.saveNow();
	}

	async renameProject(name: string) {
		const n = name.trim();
		if (!n || n === this.projectName) return;
		this.projectName = n;
		await this.saveNow();
	}

	async deleteCurrentProject() {
		const id = this.projectId;
		if (!id) return;
		clearTimeout(this.#saveTimer);
		// detach first, so a save still in flight cannot put it back in the list
		this.projectId = null;
		this.projects = this.projects.filter((p) => p.id !== id);
		await deleteProject(id).catch(() => {});
		await this.refreshProjects();
		const next = this.projects[0];
		if (next) await this.openProject(next.id);
		else await this.newProject();
	}

	/** Import a schema file as a new project. ERD-JSON keeps its canvas positions. */
	async importFile(file: File): Promise<string | null> {
		const src = await file.text();
		const name = file.name.replace(/\.[^.]+$/, '') || 'Imported';
		const from = formatFromFileName(file.name);
		if (from === 'erdjson') {
			const r = await convert(src, 'erdjson', 'dbml');
			if (!r.ok) return r.error;
			await this.newProject(name, 'dbml', r.output, JSON.parse(src));
			return null;
		}
		const r = await toErd(src, from);
		if (!r.ok) return r.error;
		const used = r.from ?? from;
		if (used === 'erdjson') {
			const back = await convert(src, 'erdjson', 'dbml');
			if (!back.ok) return back.error;
			await this.newProject(name, 'dbml', back.output, JSON.parse(src));
			return null;
		}
		const format = FORMATS.some((f) => f.id === used) ? used : 'sql';
		await this.newProject(name, format, src);
		return null;
	}

	/** Download the current schema in `format` (ERD-JSON keeps canvas positions). */
	async exportFile(format: string): Promise<string | null> {
		if (!this.erd) return 'nothing to export yet';
		let text: string;
		if (format === 'erdjson') text = JSON.stringify(this.erd, null, 2) + '\n';
		else if (format === this.format) text = this.text;
		else {
			const r = await convert(JSON.stringify(this.erd), 'erdjson', format);
			if (!r.ok) return r.error;
			text = r.output;
		}
		download(`${fileBase(this.projectName)}.${extensionFor(format)}`, text);
		return null;
	}

	/**
	 * Download the diagram as an image (#60), drawn from the workspace state so
	 * it works even when the canvas is not on screen. Explorer-hidden tables are
	 * left out.
	 */
	async exportImage(kind: 'svg' | 'png', theme: Theme = 'dark'): Promise<string | null> {
		if (!this.erd) return 'nothing to export yet';
		const hidden = this.layout === 'explorer' ? this.hidden : undefined;
		const d = diagramSvg(this.erd, { theme, hidden });
		if (!d) return 'no tables to draw';
		const base = `${fileBase(this.projectName)}${theme === 'light' ? '-light' : ''}`;
		if (kind === 'svg') {
			download(`${base}.svg`, new Blob([d.svg], { type: 'image/svg+xml;charset=utf-8' }));
			return null;
		}
		try {
			download(`${base}.png`, await svgToPng(d));
			return null;
		} catch (e) {
			return e instanceof Error ? e.message : 'could not render the image';
		}
	}

	/** Text-pane edit: debounced re-parse. */
	setText(text: string) {
		this.text = text;
		// mark dirty now (not after the debounced parse) so switching projects
		// right after typing still flushes the last keystrokes
		this.scheduleSave();
		clearTimeout(this.#timer);
		this.#timer = setTimeout(() => this.analyze(), DEBOUNCE_MS);
	}

	/** Parse `text` → erd. Positions of nodes already on the canvas survive. */
	async analyze(save = true) {
		const seq = ++this.#seq;
		const r = await toErd(this.text, this.format);
		if (seq !== this.#seq) return; // a newer edit superseded this one
		if (!r.ok) {
			this.error = r.error;
			return;
		}
		const prev = new Map(this.erd?.nodes.map((n) => [n.id, n.position]));
		const doc: ErdDoc = JSON.parse(r.output);
		for (const n of doc.nodes) n.position = prev.get(n.id) ?? n.position;
		this.erd = doc;
		this.fidelity = r.fidelity;
		this.parseMs = r.ms;
		this.error = null;
		if (save) this.scheduleSave();
	}

	/** Switch the text pane's format by re-emitting the current erd. */
	async setFormat(format: string) {
		if (!this.erd || format === this.format) return;
		const r = await convert(JSON.stringify(this.erd), 'erdjson', format);
		if (!r.ok) {
			this.error = r.error;
			return;
		}
		this.format = format;
		this.text = r.output;
		this.fidelity = r.fidelity;
		this.error = null;
		this.scheduleSave();
	}

	/** Canvas edit: regenerate text from the edited doc, then re-parse it back. */
	async commit(doc: ErdDoc) {
		this.erd = doc;
		const r = await convert(JSON.stringify(doc), 'erdjson', this.format);
		if (!r.ok) {
			this.error = r.error;
			return;
		}
		this.text = r.output;
		await this.analyze();
		this.fidelity = r.fidelity;
	}

	/** Dragging only moves pixels — no text change, no reparse. */
	move(positions: Map<string, { x: number; y: number }>) {
		if (!this.erd) return;
		this.erd = {
			...this.erd,
			meta: { ...this.erd.meta, layout: 'manual' },
			nodes: this.erd.nodes.map((n) => ({ ...n, position: positions.get(n.id) ?? n.position }))
		};
		this.scheduleSave();
	}

	addTable() {
		if (!this.erd) return;
		const used = new Set(this.erd.nodes.map((n) => n.id));
		let i = used.size + 1;
		while (used.has(`table_${i}`)) i++;
		const node: ErdNode = {
			id: `table_${i}`,
			name: `table_${i}`,
			position: { x: 40 + (i % 4) * 40, y: 40 + (i % 4) * 40 },
			columns: [{ name: 'id', type: 'integer', isPk: true, isFk: false, isUnique: false, isNullable: false }]
		};
		return this.commit({ ...this.erd, nodes: [...this.erd.nodes, node] });
	}

	/** Delete tables (and every edge touching them) and/or individual edges. */
	remove(tableIds: string[], edgeIds: string[]) {
		if (!this.erd || !(tableIds.length || edgeIds.length)) return;
		const gone = new Set(tableIds);
		const goneEdges = new Set(edgeIds);
		return this.commit({
			...this.erd,
			nodes: this.erd.nodes.filter((n) => !gone.has(n.id)),
			edges: this.erd.edges.filter(
				(e) => !goneEdges.has(e.id) && !gone.has(e.from.table) && !gone.has(e.to.table)
			)
		});
	}

	addRelation(from: { table: string; column: string }, to: { table: string; column: string }) {
		if (!this.erd) return;
		const id = `${from.table}.${from.column}->${to.table}.${to.column}`;
		if (this.erd.edges.some((e) => e.id === id)) return;
		return this.commit({
			...this.erd,
			edges: [
				...this.erd.edges,
				{
					id,
					from: { table: from.table, columns: [from.column] },
					to: { table: to.table, columns: [to.column] },
					kind: 'many-to-one'
				}
			]
		});
	}

	renameColumn(table: string, from: string, to: string) {
		if (!this.erd || !to || to === from) return;
		const rn = (t: string, c: string) => (t === table && c === from ? to : c);
		return this.commit({
			...this.erd,
			nodes: this.erd.nodes.map((n) =>
				n.id === table ? { ...n, columns: n.columns.map((c) => (c.name === from ? { ...c, name: to } : c)) } : n
			),
			edges: this.erd.edges.map((e) => ({
				...e,
				from: { ...e.from, columns: e.from.columns.map((c) => rn(e.from.table, c)) },
				to: { ...e.to, columns: e.to.columns.map((c) => rn(e.to.table, c)) }
			}))
		});
	}

	toggleHidden(id: string) {
		const next = new Set(this.hidden);
		if (!next.delete(id)) next.add(id);
		this.hidden = next;
	}
}

export const ws = new Workspace();

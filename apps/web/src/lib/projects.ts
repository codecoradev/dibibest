// Local projects: IndexedDB in the browser, no server and no account. A project
// is the text the user edits plus the ERD-JSON (which carries canvas positions).
// When IndexedDB is unavailable (some private modes), an in-memory map keeps the
// app working for the session and `persistent` reports false.
import type { ErdDoc } from './core';

export type Project = {
	id: string;
	name: string;
	format: string;
	text: string;
	erd: ErdDoc | null;
	updatedAt: number;
};

export type ProjectSummary = Pick<Project, 'id' | 'name' | 'format' | 'updatedAt'>;

const DB = 'dibibest';
const STORE = 'projects';
const LAST = 'dibibest:last-project';

let db: Promise<IDBDatabase | null> | undefined;
const memory = new Map<string, Project>();

function open(): Promise<IDBDatabase | null> {
	db ??= new Promise((resolve) => {
		try {
			const req = indexedDB.open(DB, 1);
			req.onupgradeneeded = () => req.result.createObjectStore(STORE, { keyPath: 'id' });
			req.onsuccess = () => resolve(req.result);
			req.onerror = () => resolve(null);
			req.onblocked = () => resolve(null);
		} catch {
			resolve(null);
		}
	});
	return db;
}

function tx<T>(mode: IDBTransactionMode, run: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
	return open().then(
		(d) =>
			new Promise<T>((resolve, reject) => {
				if (!d) return reject(new Error('IndexedDB unavailable'));
				const req = run(d.transaction(STORE, mode).objectStore(STORE));
				req.onsuccess = () => resolve(req.result);
				req.onerror = () => reject(req.error);
			})
	);
}

/** True when projects survive a reload (IndexedDB works). */
export async function persistent(): Promise<boolean> {
	return (await open()) !== null;
}

export async function listProjects(): Promise<ProjectSummary[]> {
	const all = (await open())
		? await tx<Project[]>('readonly', (s) => s.getAll())
		: [...memory.values()];
	return all
		.map(({ id, name, format, updatedAt }) => ({ id, name, format, updatedAt }))
		.sort((a, b) => b.updatedAt - a.updatedAt);
}

export async function getProject(id: string): Promise<Project | undefined> {
	return (await open()) ? tx<Project | undefined>('readonly', (s) => s.get(id)) : memory.get(id);
}

export async function saveProject(p: Project): Promise<void> {
	// structuredClone: IndexedDB rejects Svelte state proxies
	const plain: Project = structuredClone({ ...p, erd: p.erd ? JSON.parse(JSON.stringify(p.erd)) : null });
	if (await open()) await tx('readwrite', (s) => s.put(plain));
	else memory.set(p.id, plain);
	rememberLast(p.id);
}

export async function deleteProject(id: string): Promise<void> {
	if (await open()) await tx('readwrite', (s) => s.delete(id));
	else memory.delete(id);
}

export function newId(): string {
	// randomUUID needs a secure context (https / localhost); plain-http dev on a
	// LAN address falls back to getRandomValues, which works everywhere
	if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
	const b = crypto.getRandomValues(new Uint8Array(16));
	return Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}

export function lastProjectId(): string | null {
	try {
		return localStorage.getItem(LAST);
	} catch {
		return null;
	}
}

function rememberLast(id: string) {
	try {
		localStorage.setItem(LAST, id);
	} catch {
		/* storage blocked: the app still works, it just opens the newest project */
	}
}

/** File extension → format id for import; `auto` lets the core detect it. */
export function formatFromFileName(name: string): string {
	const ext = name.toLowerCase().split('.').pop() ?? '';
	return { dbml: 'dbml', sql: 'sql', prisma: 'prisma', json: 'erdjson' }[ext] ?? 'auto';
}

/** Format id → file extension for downloads. */
export function extensionFor(format: string): string {
	if (format.startsWith('sql')) return 'sql';
	return { dbml: 'dbml', prisma: 'prisma', erdjson: 'json' }[format] ?? 'txt';
}

/** Trigger a browser download of `text` as `fileName`. */
export function download(fileName: string, text: string): void {
	const url = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }));
	const a = Object.assign(document.createElement('a'), { href: url, download: fileName });
	document.body.append(a);
	a.click();
	a.remove();
	setTimeout(() => URL.revokeObjectURL(url), 0);
}

/** A safe file base name from a project name. */
export function fileBase(name: string): string {
	return name.trim().replace(/[^\w.-]+/g, '-').replace(/^-+|-+$/g, '') || 'schema';
}

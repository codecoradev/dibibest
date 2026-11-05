<script lang="ts">
	import { FORMATS, ws, type Layout } from '../workspace.svelte';
	import { convert } from '../core';
	import { copy } from '../clipboard';
	import { auth } from '../auth.svelte';
	import FidelityPanel from './FidelityPanel.svelte';

	const LAYOUTS: { id: Layout; label: string }[] = [
		{ id: 'text', label: '▤ Text-first' },
		{ id: 'canvas', label: '▦ Canvas-first' },
		{ id: 'explorer', label: '☰ Explorer' }
	];
	const EXPORTS = [...FORMATS, { id: 'erdjson', label: 'ERD-JSON' }];
	const DOWNLOADS = [
		{ id: 'dbml', label: 'DBML' },
		{ id: 'sql', label: 'SQL (PostgreSQL)' },
		{ id: 'sql:mysql', label: 'SQL (MySQL)' },
		{ id: 'sql:sqlite', label: 'SQL (SQLite)' },
		{ id: 'prisma', label: 'Prisma' },
		{ id: 'erdjson', label: 'ERD-JSON' }
	];
	const SAVE_LABEL = { saved: 'Saved locally', saving: 'Saving…', unsaved: 'Unsaved changes', error: 'Not saved' };

	let showFidelity = $state(false);
	let showExport = $state(false);
	let showProjects = $state(false);
	let copied = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let fileInput: HTMLInputElement;

	function flash(msg: string) {
		notice = msg;
		setTimeout(() => (notice = null), 4000);
	}

	/** Import and download need sign-in when the deployment has it enabled. */
	function gate(action: string): boolean {
		if (auth.mayTransfer) return true;
		showProjects = false;
		showExport = false;
		flash(`Sign in with CodeCora ID to ${action}. Redirecting…`);
		setTimeout(() => auth.signIn('/app'), 900);
		return false;
	}

	function chooseFile() {
		if (gate('import files')) fileInput.click();
	}

	async function importChosen(e: Event) {
		const input = e.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		input.value = '';
		showProjects = false;
		if (!file) return;
		const err = await ws.importFile(file);
		flash(err ? `Import failed: ${err}` : `Imported ${file.name}`);
	}

	async function downloadAs(to: string) {
		if (!gate('download files')) return;
		showExport = false;
		const err = await ws.exportFile(to);
		if (err) flash(`Export failed: ${err}`);
	}

	async function rename() {
		showProjects = false;
		const name = prompt('Project name', ws.projectName);
		if (name !== null) await ws.renameProject(name);
	}

	async function remove() {
		showProjects = false;
		if (confirm(`Delete "${ws.projectName}" from this browser? This cannot be undone.`)) await ws.deleteCurrentProject();
	}

	async function create() {
		showProjects = false;
		await ws.newProject();
	}

	async function open(id: string) {
		showProjects = false;
		await ws.openProject(id);
	}

	// Chip numbers come straight from the core Fidelity struct.
	const chip = $derived.by(() => {
		const f = ws.fidelity;
		if (!f) return null;
		const parts = [f.tables, f.columns, f.refs, f.indexes];
		const kept = parts.reduce((a, o) => a + o.kept, 0);
		const total = parts.reduce((a, o) => a + o.total, 0);
		const lossless = f.idempotent && kept === total && !f.dropped?.length;
		const pct = total ? Math.floor((kept / total) * 100) : 100;
		return { lossless, text: lossless ? 'round-trip 100% lossless' : `round-trip ${pct}% · ${f.dropped?.length ?? 0} dropped` };
	});

	async function exportAs(to: string) {
		if (!ws.erd) return;
		const r = to === ws.format ? { ok: true as const, output: ws.text } : await convert(JSON.stringify(ws.erd), 'erdjson', to);
		if (!r.ok) return (ws.error = r.error);
		copied = (await copy(r.output)) ? to : null;
		showExport = false;
		setTimeout(() => (copied = null), 2000);
	}
</script>

<header class="topbar">
	<div class="brand">
		<span class="mark">db</span> dibibest
		<div class="projects">
			<button
				class="project-name"
				aria-haspopup="menu"
				aria-expanded={showProjects}
				aria-label="Projects: {ws.projectName}"
				onclick={() => (showProjects = !showProjects)}>/ {ws.projectName} ▾</button
			>
			{#if showProjects}
				<div class="menu left" role="menu" aria-label="Projects">
					<button role="menuitem" onclick={create}>＋ New project</button>
					<button role="menuitem" onclick={chooseFile}>⇪ Import file…{#if !auth.mayTransfer}<span class="fmt">sign in</span>{/if}</button>
					<button role="menuitem" onclick={rename}>✎ Rename…</button>
					<button role="menuitem" class="danger" onclick={remove}>✕ Delete…</button>
					{#if ws.projects.length}
						<div class="sep" role="separator"></div>
						<div class="menu-label">Saved in this browser</div>
						{#each ws.projects as p (p.id)}
							<button role="menuitem" aria-current={p.id === ws.projectId} onclick={() => open(p.id)}>
								{p.name}<span class="fmt">{p.format}</span>
							</button>
						{/each}
					{/if}
				</div>
			{/if}
		</div>
		<input
			bind:this={fileInput}
			type="file"
			accept=".dbml,.sql,.prisma,.json,.txt"
			aria-label="Import schema file"
			hidden
			onchange={importChosen}
		/>
	</div>
	<span class="save" class:warn={ws.saveState === 'error' || !ws.persistent} title={ws.persistent ? '' : 'Browser storage is unavailable: projects last for this session only'}>
		{ws.persistent ? SAVE_LABEL[ws.saveState] : 'Session only'}
	</span>
	{#if ws.layout === 'text'}
		<div class="seg" role="tablist" aria-label="Source format">
			{#each FORMATS as f (f.id)}
				<button role="tab" aria-selected={ws.format === f.id} onclick={() => ws.setFormat(f.id)}>{f.label}</button>
			{/each}
		</div>
	{/if}
	<div class="seg layout" role="tablist" aria-label="Workspace layout">
		{#each LAYOUTS as l (l.id)}
			<button role="tab" aria-selected={ws.layout === l.id} onclick={() => (ws.layout = l.id)}>{l.label}</button>
		{/each}
	</div>
	<div class="spacer"></div>
	{#if chip}
		<button
			class="chip"
			class:lossy={!chip.lossless}
			aria-expanded={showFidelity}
			onclick={() => (showFidelity = !showFidelity)}>◈ {chip.text}</button
		>
	{/if}
	<div class="export">
		<button class="btn-primary" aria-expanded={showExport} onclick={() => (showExport = !showExport)}>
			{copied ? `Copied ${copied} ✓` : 'Export ▾'}
		</button>
		{#if showExport}
			<div class="menu" role="menu">
				{#each EXPORTS as f (f.id)}
					<button role="menuitem" onclick={() => exportAs(f.id)}>Copy as {f.label}</button>
				{/each}
				<div class="sep" role="separator"></div>
				{#each DOWNLOADS as f (f.id)}
					<button role="menuitem" onclick={() => downloadAs(f.id)}
						>Download {f.label}{#if !auth.mayTransfer}<span class="fmt">sign in</span>{/if}</button
					>
				{/each}
			</div>
		{/if}
	</div>
	{#if auth.enabled}
		{#if auth.user}
			<form method="POST" action="/auth/logout" class="account">
				<span class="who" title={auth.user.email}>{auth.user.name ?? auth.user.email}</span>
				<button type="submit" class="link">Sign out</button>
			</form>
		{:else}
			<button class="link" onclick={() => auth.signIn('/app')}>Sign in</button>
		{/if}
	{/if}
	{#if showFidelity}<FidelityPanel />{/if}
	{#if notice}<div class="notice" role="status">{notice}</div>{/if}
</header>

<style>
	.topbar {
		position: relative;
		height: 48px;
		flex: 0 0 48px;
		background: var(--bg-1);
		border-bottom: 1px solid var(--line);
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		padding: 0 var(--sp-4);
	}
	.brand {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
		font-weight: 650;
	}
	.mark {
		width: 22px;
		height: 22px;
		border-radius: var(--r-sm);
		background: linear-gradient(135deg, var(--accent), #8a5cff);
		display: grid;
		place-items: center;
		font-size: 12px;
		font-weight: 800;
		color: #0b0d10;
	}
	.projects {
		position: relative;
	}
	.project-name {
		color: var(--text-2);
		font-weight: 500;
		padding: 4px 6px;
		border-radius: var(--r-sm);
	}
	.project-name:hover {
		background: var(--bg-2);
		color: var(--text-1);
	}
	.save {
		font-size: 12px;
		color: var(--text-3, var(--text-2));
		white-space: nowrap;
	}
	.save.warn {
		color: var(--warn);
	}
	.menu.left {
		left: 0;
		right: auto;
		min-width: 240px;
		max-height: 70vh;
		overflow-y: auto;
	}
	.menu .sep {
		height: 1px;
		background: var(--line);
		margin: var(--sp-1) 0;
	}
	.menu-label {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-2);
		padding: 4px var(--sp-3);
	}
	.menu button[aria-current='true'] {
		color: var(--text-1);
		background: var(--bg-3);
	}
	.menu .fmt {
		float: right;
		font-size: 11px;
		color: var(--text-2);
		margin-left: var(--sp-3);
	}
	.menu .danger:hover {
		color: var(--err, #ff6b6b);
	}
	.account {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
	}
	.who {
		max-width: 160px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 12.5px;
		color: var(--text-2);
	}
	.link {
		font-size: 13px;
		color: var(--text-2);
		padding: 5px 8px;
		border-radius: var(--r-sm);
	}
	.link:hover {
		color: var(--text-1);
		background: var(--bg-2);
	}
	.notice {
		position: absolute;
		top: 52px;
		left: 50%;
		transform: translateX(-50%);
		z-index: 40;
		background: var(--bg-2);
		border: 1px solid var(--line);
		border-radius: var(--r-md);
		padding: 6px 14px;
		font-size: 13px;
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
	}
	.seg {
		display: flex;
		gap: 2px;
		background: var(--bg-2);
		border: 1px solid var(--line);
		border-radius: var(--r-md);
		padding: 2px;
	}
	.seg button {
		padding: 5px 14px;
		border-radius: 8px;
		color: var(--text-2);
		font-weight: 550;
		font-size: 13px;
	}
	.seg button[aria-selected='true'] {
		background: var(--bg-3);
		color: var(--text-1);
		box-shadow: inset 0 0 0 1px var(--line);
	}
	.seg.layout button[aria-selected='true'] {
		background: var(--accent-strong);
		color: #fff;
		box-shadow: none;
	}
	.spacer {
		flex: 1;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font-size: 12.5px;
		font-weight: 600;
		color: var(--ok);
		background: rgba(62, 207, 142, 0.09);
		border: 1px solid rgba(62, 207, 142, 0.25);
		padding: 4px 10px;
		border-radius: 999px;
		font-variant-numeric: tabular-nums;
	}
	.chip.lossy {
		color: var(--warn);
		background: rgba(245, 181, 68, 0.09);
		border-color: rgba(245, 181, 68, 0.3);
	}
	.btn-primary {
		background: var(--accent-strong);
		color: #fff;
		font-weight: 600;
		font-size: 13.5px;
		padding: 7px 16px;
		border-radius: var(--r-md);
	}
	.export {
		position: relative;
	}
	.menu {
		position: absolute;
		right: 0;
		top: 40px;
		z-index: 30;
		min-width: 180px;
		display: flex;
		flex-direction: column;
		background: var(--bg-2);
		border: 1px solid var(--line);
		border-radius: var(--r-md);
		padding: var(--sp-1);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
	}
	.menu button {
		white-space: nowrap;
		text-align: left;
		padding: 7px var(--sp-3);
		border-radius: var(--r-sm);
		color: var(--text-2);
	}
	.menu button:hover {
		background: var(--bg-3);
		color: var(--text-1);
	}
</style>

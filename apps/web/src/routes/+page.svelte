<script lang="ts">
	import { onMount } from 'svelte';
	import { convert, type Fidelity } from '../lib/core';

	const REPO = 'https://github.com/codecoradev/dibibest';

	// ── live demo: the real core (WASM) converts as you type ────────────────
	const SAMPLE = `Table users {
  id integer [pk, increment]
  email varchar [not null, unique]
  plan varchar [default: 'free']
}

Table orders {
  id integer [pk]
  user_id integer [not null]
  total_cents integer
}

Ref: orders.user_id > users.id [delete: cascade]
`;
	const TARGETS = [
		{ id: 'sql', label: 'PostgreSQL' },
		{ id: 'sql:mysql', label: 'MySQL' },
		{ id: 'sql:sqlite', label: 'SQLite' },
		{ id: 'prisma', label: 'Prisma' }
	];

	let src = $state(SAMPLE);
	let target = $state('sql');
	let output = $state('');
	let fidelity = $state<Fidelity | null>(null);
	let error = $state<string | null>(null);
	let ms = $state(0);
	let ready = $state(false);
	let run = 0;

	$effect(() => {
		const text = src;
		const to = target;
		const id = ++run;
		const t = setTimeout(async () => {
			const r = await convert(text, 'dbml', to);
			if (id !== run) return;
			ready = true;
			if (r.ok) {
				output = r.output;
				fidelity = r.fidelity;
				ms = r.ms;
				error = null;
			} else {
				error = r.error;
			}
		}, 120);
		return () => clearTimeout(t);
	});

	const lossless = $derived(
		!!fidelity &&
			fidelity.idempotent &&
			!fidelity.dropped?.length &&
			[fidelity.tables, fidelity.columns, fidelity.refs, fidelity.indexes].every((o) => o.kept === o.total)
	);

	// ── static content ──────────────────────────────────────────────────────
	const FORMATS = [
		{ name: 'DBML', note: 'dbdiagram.io syntax: tables, refs, enums, indexes, notes' },
		{ name: 'PostgreSQL', note: 'CREATE TABLE / INDEX / TYPE … AS ENUM, FK actions' },
		{ name: 'MySQL', note: 'backtick quoting, AUTO_INCREMENT' },
		{ name: 'SQLite', note: 'AUTOINCREMENT, partial and expression indexes' },
		{ name: 'Prisma', note: 'models, @id, @unique, @default, @relation, enums' },
		{ name: 'ERD-JSON', note: 'the canvas format, with table positions' }
	];

	const MCP_LOCAL = `{
  "mcpServers": {
    "dibibest": { "command": "dibibest-mcp" }
  }
}`;
	const MCP_REMOTE = `{
  "mcpServers": {
    "dibibest": { "url": "https://dibibest.codecora.dev/mcp" }
  }
}`;
	const CLI = `cargo install --git ${REPO} dibibest-cli

dibibest convert --to sql schema.dbml
cat schema.sql | dibibest convert --to prisma`;

	let mcpTab = $state<'remote' | 'local' | 'cli'>('remote');
	const snippet = $derived(mcpTab === 'remote' ? MCP_REMOTE : mcpTab === 'local' ? MCP_LOCAL : CLI);

	onMount(() => {
		// reveal-on-scroll, skipped when the user prefers less motion
		if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;
		const els = document.querySelectorAll<HTMLElement>('[data-reveal]');
		els.forEach((el) => el.classList.add('pre'));
		const io = new IntersectionObserver(
			(entries) => {
				for (const e of entries) {
					if (e.isIntersecting) {
						e.target.classList.remove('pre');
						io.unobserve(e.target);
					}
				}
			},
			{ rootMargin: '0px 0px -10% 0px' }
		);
		els.forEach((el) => io.observe(el));
		return () => io.disconnect();
	});
</script>

<svelte:head>
	<title>dibibest: database schemas, both ways</title>
	<meta
		name="description"
		content="Convert database schemas between DBML, PostgreSQL, MySQL, SQLite, Prisma and an interactive ERD, with a fidelity report on every conversion. Runs in your browser."
	/>
	<meta property="og:title" content="dibibest: database schemas, both ways" />
	<meta property="og:description" content="DBML ⇄ SQL ⇄ Prisma ⇄ ERD, with proof of what survived the round trip." />
	<meta property="og:image" content="https://dibibest.codecora.dev/img/workspace.webp" />
</svelte:head>

<div class="page">
	<nav class="nav" aria-label="Main">
		<a class="logo" href="/" aria-label="dibibest home"><span class="mark">db</span> dibibest</a>
		<div class="links">
			<a href="#fidelity">Fidelity</a>
			<a href="#formats">Formats</a>
			<a href="#privacy">Privacy</a>
			<a href="#agents">MCP</a>
			<a href={REPO}>GitHub</a>
		</div>
		<a class="btn small" href="/app">Open app</a>
	</nav>

	<header class="hero">
		<div class="glow" aria-hidden="true"></div>
		<div class="hero-copy">
			<p class="eyebrow"><span class="dot"></span> Public beta · open source · MIT</p>
			<h1>Database schemas,<br /><span class="grad">both ways.</span></h1>
			<p class="lede">
				Convert between DBML, PostgreSQL, MySQL, SQLite, Prisma and a live ERD. Every conversion
				comes with a <strong>fidelity report</strong> that shows what made it across and what didn't.
			</p>
			<div class="ctas">
				<a class="btn" href="/app">Open the workspace →</a>
				<a class="btn ghost" href={REPO}>Star on GitHub</a>
			</div>
			<p class="fine">No upload: conversion runs in your browser on a WebAssembly build of the core.</p>
		</div>

		<section class="demo" aria-label="Live conversion demo">
			<div class="demo-bar">
				<span class="dots" aria-hidden="true"><i></i><i></i><i></i></span>
				<span class="demo-title">live demo: edit the DBML</span>
				{#if fidelity}
					<span class="chip" class:lossy={!lossless}>◈ {lossless ? '100% lossless' : 'lossy'}</span>
				{/if}
			</div>
			<div class="demo-body">
				<label class="pane">
					<span class="pane-h">schema.dbml</span>
					<textarea bind:value={src} spellcheck="false" aria-label="DBML input"></textarea>
				</label>
				<div class="pane out">
					<div class="tabs" role="tablist" aria-label="Target format">
						{#each TARGETS as t (t.id)}
							<button role="tab" aria-selected={target === t.id} onclick={() => (target = t.id)}>{t.label}</button>
						{/each}
					</div>
					<pre aria-live="polite" aria-label="Converted output">{#if error}<span class="err">✗ {error}</span>{:else if ready}{output}{:else}<span class="muted">loading the core…</span>{/if}</pre>
				</div>
			</div>
			<div class="demo-foot">
				{#if fidelity}
					<span>tables <b>{fidelity.tables.kept}/{fidelity.tables.total}</b></span>
					<span>columns <b>{fidelity.columns.kept}/{fidelity.columns.total}</b></span>
					<span>refs <b>{fidelity.refs.kept}/{fidelity.refs.total}</b></span>
					<span>indexes <b>{fidelity.indexes.kept}/{fidelity.indexes.total}</b></span>
					<span class="ms">{ms.toFixed(1)} ms</span>
				{:else}
					<span class="muted">fidelity report appears here</span>
				{/if}
			</div>
		</section>
	</header>

	<section class="shot" data-reveal>
		<img
			src="/img/workspace.webp"
			alt="The dibibest workspace: DBML editor on the left, live ERD on the right, round-trip fidelity chip in the header"
			width="2560"
			height="1520"
			loading="lazy"
		/>
	</section>

	<section id="fidelity" class="section split" data-reveal>
		<div>
			<p class="kicker">Fidelity report</p>
			<h2>Every conversion comes with a receipt.</h2>
			<p>
				Most converters quietly drop partial indexes, enums, cascades and notes. dibibest counts them every
				time, so you know before you ship the migration.
			</p>
			<ul class="checks">
				<li><b>Kept / total per category.</b> Totals are counted from the source text, so anything the parser misses still shows up.</li>
				<li><b>Matched by content.</b> A reversed ref or a missing <code>ON DELETE CASCADE</code> does not count as kept.</li>
				<li><b>Dropped, by name.</b> Anything the target format can't express is listed by name.</li>
				<li><b>Round-trip tested.</b> <code>parse(emit(parse(src))) == parse(src)</code> is checked on every conversion and in CI.</li>
			</ul>
		</div>
		<figure class="report">
			<figcaption>-- fidelity --</figcaption>
			<div class="row"><span>tables</span><span>2/2 <i>✓</i></span></div>
			<div class="row"><span>columns</span><span>6/6 <i>✓</i></span></div>
			<div class="row"><span>refs</span><span>1/1 <i>✓</i></span></div>
			<div class="row"><span>indexes</span><span>1/2 <i class="warn">!</i></span></div>
			<div class="row"><span>round-trip</span><span>lossless <i>✓</i></span></div>
			<div class="dropped">dropped: index <code>users_lower_email</code> (expression index, no MySQL equivalent)</div>
		</figure>
	</section>

	<section id="formats" class="section" data-reveal>
		<p class="kicker">Formats</p>
		<h2>One internal model. Six formats, read and write.</h2>
		<div class="grid">
			{#each FORMATS as f (f.name)}
				<div class="card">
					<div class="card-h"><span>{f.name}</span><span class="pill">parse · emit</span></div>
					<p>{f.note}</p>
				</div>
			{/each}
		</div>
		<p class="note">
			Design-first like dbdiagram, reverse-first from your SQL, or start from Prisma. Switch between text-first,
			canvas-first and explorer layouts without losing anything.
		</p>
	</section>

	<section id="privacy" class="section split reverse" data-reveal>
		<figure class="store" aria-hidden="true">
			<div class="device">
				<div class="device-h">your browser</div>
				<div class="layer">workspace UI</div>
				<div class="arrow">↕</div>
				<div class="layer core">dibibest core · WebAssembly</div>
				<div class="arrow">↕</div>
				<div class="layer db">IndexedDB · your projects</div>
			</div>
			<div class="cloud">
				<div class="cloud-h">our servers</div>
				<p>no database<br />no schema uploads</p>
				<p class="small">only sign-in, via CodeCora ID</p>
			</div>
		</figure>
		<div>
			<p class="kicker">Privacy</p>
			<h2>Your schemas stay on your device.</h2>
			<p>
				Projects autosave to <b>IndexedDB</b>, the database built into your browser. They live on your device and
				never leave it. We run no database and keep no copies.
			</p>
			<ul class="checks">
				<li><b>Editing is free and needs no account:</b> type, convert, inspect the diagram and the fidelity report.</li>
				<li><b>Import, copy and download</b> ask you to sign in with CodeCora ID (Google). That's how we count who uses dibibest.</li>
				<li><b>Want a backup or another device?</b> Download your project. Clearing site data removes local projects.</li>
			</ul>
		</div>
	</section>

	<section id="agents" class="section split" data-reveal>
		<div>
			<p class="kicker">For AI agents</p>
			<h2>An MCP server your agent can call.</h2>
			<p>
				<code>convert_schema</code> and <code>list_formats</code> return the converted text along with the
				fidelity report, so an agent can see what got lost. Run it locally with no auth, or use the hosted
				endpoint with OAuth sign-in (no API keys).
			</p>
		</div>
		<div class="code">
			<div class="tabs" role="tablist" aria-label="Setup">
				<button role="tab" aria-selected={mcpTab === 'remote'} onclick={() => (mcpTab = 'remote')}>Remote MCP</button>
				<button role="tab" aria-selected={mcpTab === 'local'} onclick={() => (mcpTab = 'local')}>Local MCP</button>
				<button role="tab" aria-selected={mcpTab === 'cli'} onclick={() => (mcpTab = 'cli')}>CLI</button>
			</div>
			<pre>{snippet}</pre>
		</div>
	</section>

	<section class="final" data-reveal>
		<h2>Paste a schema and see what survives.</h2>
		<a class="btn" href="/app">Open the workspace →</a>
	</section>

	<footer class="footer">
		<span><span class="mark sm">db</span> dibibest · MIT · by CodeCora</span>
		<span class="flinks">
			<a href={REPO}>GitHub</a>
			<a href="{REPO}/issues/new">Report a bug</a>
			<a href="{REPO}#keterbatasan-beta">Beta limits</a>
		</span>
	</footer>
</div>

<style>
	.page {
		--max: 1160px;
		overflow-x: clip;
	}
	a {
		color: inherit;
	}
	code {
		font-family: var(--mono);
		font-size: 0.88em;
		background: var(--bg-3);
		padding: 1px 5px;
		border-radius: 4px;
	}

	/* nav */
	.nav {
		position: sticky;
		top: 0;
		z-index: 10;
		display: flex;
		align-items: center;
		gap: var(--sp-4);
		max-width: var(--max);
		margin: 0 auto;
		padding: var(--sp-3) var(--sp-4);
		backdrop-filter: blur(12px);
		background: rgba(11, 13, 16, 0.72);
	}
	.logo {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
		font-weight: 700;
		text-decoration: none;
	}
	.mark {
		width: 24px;
		height: 24px;
		border-radius: var(--r-sm);
		background: linear-gradient(135deg, var(--accent), #8a5cff);
		display: inline-grid;
		place-items: center;
		font-size: 12px;
		font-weight: 800;
		color: #0b0d10;
	}
	.mark.sm {
		width: 18px;
		height: 18px;
		font-size: 10px;
		vertical-align: -3px;
	}
	.links {
		display: none;
		gap: var(--sp-4);
		margin-left: auto;
		font-size: 14px;
		color: var(--text-2);
	}
	.links a {
		text-decoration: none;
	}
	.links a:hover {
		color: var(--text-1);
	}
	.nav .btn.small {
		margin-left: auto;
	}

	/* buttons */
	.btn {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		background: var(--accent-strong);
		color: #fff;
		font-weight: 650;
		padding: 12px 20px;
		border-radius: var(--r-md);
		text-decoration: none;
		transition:
			transform 0.15s,
			background 0.15s;
	}
	.btn:hover {
		background: #3b74f0;
		transform: translateY(-1px);
	}
	.btn.small {
		padding: 7px 14px;
		font-size: 14px;
	}
	.btn.ghost {
		background: transparent;
		border: 1px solid var(--line);
		color: var(--text-1);
	}
	.btn.ghost:hover {
		background: var(--bg-2);
	}

	/* hero */
	.hero {
		position: relative;
		max-width: var(--max);
		margin: 0 auto;
		padding: 48px var(--sp-4) 32px;
		display: grid;
		gap: 40px;
	}
	.glow {
		position: absolute;
		inset: -120px -20% auto;
		height: 520px;
		background:
			radial-gradient(40% 50% at 30% 40%, rgba(79, 140, 255, 0.22), transparent 70%),
			radial-gradient(35% 45% at 75% 30%, rgba(138, 92, 255, 0.18), transparent 70%);
		pointer-events: none;
		z-index: -1;
	}
	.eyebrow {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-size: 13px;
		color: var(--text-2);
		border: 1px solid var(--line);
		background: var(--bg-1);
		padding: 5px 12px;
		border-radius: 999px;
	}
	.dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--ok);
		box-shadow: 0 0 0 4px rgba(62, 207, 142, 0.15);
	}
	h1 {
		margin: 20px 0 16px;
		font-size: clamp(40px, 9vw, 68px);
		line-height: 1.02;
		letter-spacing: -0.035em;
		font-weight: 750;
	}
	.grad {
		background: linear-gradient(90deg, #6ea8ff, #b48cff 60%, #3ecf8e);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	.lede {
		font-size: clamp(16px, 2.4vw, 19px);
		line-height: 1.6;
		color: var(--text-2);
		max-width: 34em;
	}
	.lede strong {
		color: var(--text-1);
	}
	.ctas {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-3);
		margin: 28px 0 14px;
	}
	.fine {
		font-size: 13px;
		color: var(--text-3);
	}

	/* demo */
	.demo {
		background: var(--bg-1);
		border: 1px solid var(--line);
		border-radius: 14px;
		box-shadow:
			0 30px 80px -20px rgba(0, 0, 0, 0.7),
			0 0 0 1px rgba(79, 140, 255, 0.06);
		overflow: hidden;
		min-width: 0;
	}
	.demo-bar {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		padding: 10px var(--sp-3);
		border-bottom: 1px solid var(--line);
		font-size: 12.5px;
		color: var(--text-3);
	}
	.dots {
		display: flex;
		gap: 6px;
	}
	.dots i {
		width: 10px;
		height: 10px;
		border-radius: 50%;
		background: var(--bg-3);
	}
	.demo-title {
		font-family: var(--mono);
	}
	.chip {
		margin-left: auto;
		font-weight: 650;
		color: var(--ok);
		background: rgba(62, 207, 142, 0.09);
		border: 1px solid rgba(62, 207, 142, 0.25);
		padding: 3px 9px;
		border-radius: 999px;
		white-space: nowrap;
	}
	.chip.lossy {
		color: var(--warn);
		background: rgba(245, 181, 68, 0.09);
		border-color: rgba(245, 181, 68, 0.3);
	}
	.demo-body {
		display: grid;
	}
	.pane {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}
	.pane-h {
		font-size: 11px;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-3);
		padding: 10px var(--sp-3) 0;
		font-weight: 650;
	}
	textarea,
	pre {
		font-family: var(--mono);
		font-size: 13px;
		line-height: 1.6;
		color: var(--text-1);
	}
	textarea {
		height: 260px;
		resize: none;
		background: transparent;
		border: none;
		padding: 8px var(--sp-3) var(--sp-3);
		outline: none;
		tab-size: 2;
	}
	.out {
		border-top: 1px solid var(--line);
		background: var(--bg-0);
	}
	.out pre {
		height: 260px;
		overflow: auto;
		padding: var(--sp-3);
		white-space: pre;
	}
	.tabs {
		display: flex;
		gap: 2px;
		padding: 6px;
		border-bottom: 1px solid var(--line-soft);
		overflow-x: auto;
		scrollbar-width: none;
	}
	.tabs button {
		padding: 6px 11px;
		border-radius: 7px;
		color: var(--text-2);
		font-size: 12.5px;
		font-weight: 600;
		white-space: nowrap;
	}
	.tabs button[aria-selected='true'] {
		background: var(--bg-3);
		color: var(--text-1);
		box-shadow: inset 0 0 0 1px var(--line);
	}
	.demo-foot {
		display: flex;
		flex-wrap: wrap;
		gap: 6px var(--sp-4);
		padding: 10px var(--sp-3);
		border-top: 1px solid var(--line);
		font-family: var(--mono);
		font-size: 12px;
		color: var(--text-3);
	}
	.demo-foot b {
		color: var(--ok);
		font-weight: 600;
	}
	.ms {
		margin-left: auto;
	}
	.muted {
		color: var(--text-3);
	}
	.err {
		color: var(--warn);
		white-space: pre-wrap;
	}

	/* screenshot */
	.shot {
		max-width: var(--max);
		margin: 24px auto 0;
		padding: 0 var(--sp-4);
	}
	.shot img {
		display: block;
		width: 100%;
		height: auto;
		border-radius: 14px;
		border: 1px solid var(--line);
		box-shadow: 0 40px 120px -30px rgba(79, 140, 255, 0.25);
	}

	/* sections */
	.section {
		max-width: var(--max);
		margin: 0 auto;
		padding: 88px var(--sp-4) 0;
	}
	.split {
		display: grid;
		gap: 40px;
		align-items: center;
	}
	.kicker {
		font-size: 13px;
		font-weight: 700;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--accent);
		margin-bottom: 10px;
	}
	h2 {
		font-size: clamp(28px, 5vw, 40px);
		line-height: 1.12;
		letter-spacing: -0.025em;
		margin-bottom: 16px;
	}
	.section p {
		color: var(--text-2);
		line-height: 1.65;
		font-size: 16px;
	}
	.section p b {
		color: var(--text-1);
	}
	.checks {
		list-style: none;
		margin-top: 20px;
		display: grid;
		gap: 12px;
	}
	.checks li {
		position: relative;
		padding-left: 28px;
		color: var(--text-2);
		line-height: 1.55;
	}
	.checks li::before {
		content: '✓';
		position: absolute;
		left: 0;
		top: 1px;
		width: 18px;
		height: 18px;
		border-radius: 50%;
		display: grid;
		place-items: center;
		font-size: 11px;
		color: var(--ok);
		background: rgba(62, 207, 142, 0.12);
	}
	.checks b {
		color: var(--text-1);
	}

	/* fidelity report mock */
	.report {
		font-family: var(--mono);
		font-size: 14px;
		background: var(--bg-1);
		border: 1px solid var(--line);
		border-radius: 14px;
		padding: 20px 22px;
		box-shadow: 0 30px 80px -30px rgba(0, 0, 0, 0.7);
	}
	.report figcaption {
		color: var(--text-3);
		margin-bottom: 12px;
	}
	.report .row {
		display: flex;
		justify-content: space-between;
		padding: 7px 0;
		border-bottom: 1px dashed var(--line-soft);
		color: var(--text-2);
	}
	.report i {
		font-style: normal;
		color: var(--ok);
		margin-left: 6px;
	}
	.report i.warn {
		color: var(--warn);
	}
	.dropped {
		margin-top: 14px;
		font-size: 12.5px;
		color: var(--warn);
		line-height: 1.5;
	}

	/* formats */
	.grid {
		display: grid;
		gap: var(--sp-3);
		margin-top: 28px;
	}
	.card {
		background: var(--bg-1);
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 18px;
		transition:
			border-color 0.2s,
			transform 0.2s;
	}
	.card:hover {
		border-color: #34405a;
		transform: translateY(-2px);
	}
	.card-h {
		display: flex;
		justify-content: space-between;
		align-items: center;
		font-weight: 700;
		margin-bottom: 6px;
	}
	.pill {
		font-size: 11px;
		font-weight: 600;
		color: var(--accent);
		background: var(--accent-soft);
		padding: 2px 8px;
		border-radius: 999px;
	}
	.card p {
		font-size: 14px;
	}
	.note {
		margin-top: 20px;
	}

	/* privacy diagram */
	.store {
		display: grid;
		gap: var(--sp-3);
	}
	.device,
	.cloud {
		border: 1px solid var(--line);
		border-radius: 14px;
		padding: 18px;
		background: var(--bg-1);
	}
	.device-h,
	.cloud-h {
		font-size: 12px;
		font-weight: 700;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-3);
		margin-bottom: 12px;
	}
	.layer {
		border: 1px solid var(--line);
		border-radius: 10px;
		padding: 10px 14px;
		background: var(--bg-2);
		font-weight: 600;
		font-size: 14px;
	}
	.layer.core {
		border-color: rgba(79, 140, 255, 0.4);
		background: var(--accent-soft);
	}
	.layer.db {
		border-color: rgba(62, 207, 142, 0.35);
		background: rgba(62, 207, 142, 0.08);
		color: var(--ok);
	}
	.arrow {
		text-align: center;
		color: var(--text-3);
		padding: 4px 0;
	}
	.cloud {
		border-style: dashed;
		text-align: center;
	}
	.cloud p {
		color: var(--text-2);
		line-height: 1.6;
		font-size: 14px;
	}
	.cloud .small {
		margin-top: 6px;
		font-size: 12.5px;
		color: var(--text-3);
	}

	/* code */
	.code {
		background: var(--bg-1);
		border: 1px solid var(--line);
		border-radius: 14px;
		overflow: hidden;
		min-width: 0;
	}
	.code pre {
		padding: 18px;
		overflow-x: auto;
		min-height: 180px;
	}

	/* final + footer */
	.final {
		max-width: var(--max);
		margin: 96px auto 0;
		padding: 56px var(--sp-4);
		text-align: center;
		border-top: 1px solid var(--line-soft);
		background: radial-gradient(50% 80% at 50% 0%, rgba(79, 140, 255, 0.12), transparent 70%);
	}
	.footer {
		max-width: var(--max);
		margin: 0 auto;
		padding: 24px var(--sp-4) 40px;
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-3);
		justify-content: space-between;
		font-size: 13px;
		color: var(--text-3);
		border-top: 1px solid var(--line-soft);
	}
	.flinks {
		display: flex;
		gap: var(--sp-4);
	}
	.flinks a {
		text-decoration: none;
	}
	.flinks a:hover {
		color: var(--text-1);
	}

	/* reveal */
	:global([data-reveal]) {
		transition:
			opacity 0.6s ease,
			transform 0.6s ease;
	}
	:global([data-reveal].pre) {
		opacity: 0;
		transform: translateY(18px);
	}

	/* phones: 16px input text stops iOS from zooming in on focus */
	@media (max-width: 719px) {
		textarea {
			font-size: 16px;
		}
	}

	/* wider screens */
	@media (min-width: 720px) {
		.links {
			display: flex;
		}
		.nav .btn.small {
			margin-left: var(--sp-2);
		}
		.demo-body {
			grid-template-columns: 1fr 1fr;
		}
		.out {
			border-top: none;
			border-left: 1px solid var(--line);
		}
		textarea,
		.out pre {
			height: 320px;
		}
		.grid {
			grid-template-columns: repeat(2, 1fr);
		}
		.store {
			grid-template-columns: 1.4fr 1fr;
			align-items: center;
		}
	}
	@media (min-width: 1000px) {
		.hero {
			grid-template-columns: 1fr 1.15fr;
			align-items: center;
			padding-top: 72px;
		}
		.split {
			grid-template-columns: 1fr 1fr;
			gap: 64px;
		}
		.split.reverse > :first-child {
			order: 0;
		}
		.grid {
			grid-template-columns: repeat(3, 1fr);
		}
	}
</style>

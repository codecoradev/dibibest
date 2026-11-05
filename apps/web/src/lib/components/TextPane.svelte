<script lang="ts">
	import Editor from './Editor.svelte';
	import { ws } from '../workspace.svelte';
	import { copy } from '../clipboard';
	import { auth } from '../auth.svelte';

	// editable: text-first input pane. Otherwise: generated, read-only output (canvas-first).
	let { editable }: { editable: boolean } = $props();

	let copied = $state(false);
	async function doCopy() {
		// copying out of the workspace needs sign-in, like import and download
		if (!auth.mayTransfer) return auth.signIn('/app');
		copied = await copy(ws.text);
		setTimeout(() => (copied = false), 1500);
	}
</script>

<section class="pane" class:editable aria-label={editable ? 'Text editor — primary input' : 'Generated schema — live output'}>
	<div class="title">
		<span>{editable ? '' : 'generated '}schema.{ws.format}</span>
		{#if editable}
			<span class="state live">● autosync {Math.round(ws.parseMs)}ms</span>
		{:else}
			<button class="copy" onclick={doCopy}>{copied ? 'Copied ✓' : auth.mayTransfer ? 'Copy' : 'Sign in to copy'}</button>
		{/if}
	</div>
	<Editor value={ws.text} readonly={!editable} label="{ws.format} source" onchange={(v) => ws.setText(v)} />
	<div class="foot">
		{#if ws.error}
			<span class="bad">✗ {ws.error}</span>
		{:else if editable}
			<span class="ok">✓ valid {ws.format.toUpperCase()}</span>
			<span>{ws.erd?.nodes.length ?? 0} tables · {ws.erd?.edges.length ?? 0} refs</span>
			<span>parse {Math.round(ws.parseMs)}ms</span>
		{:else}
			<span class="ok">✓ auto-generated from canvas</span> · copy anytime · switch to Text-first to edit text-side
		{/if}
	</div>
</section>

<style>
	.pane {
		width: 40%;
		flex: 0 0 40%;
		background: var(--bg-1);
		border-left: 1px solid var(--line);
		display: flex;
		flex-direction: column;
		min-height: 0;
	}
	.pane.editable {
		width: 42%;
		flex-basis: 42%;
		border-left: none;
		border-right: 1px solid var(--line);
	}
	.title {
		font-size: 11.5px;
		font-weight: 650;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-3);
		padding: var(--sp-3) var(--sp-4);
		border-bottom: 1px solid var(--line-soft);
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.state {
		text-transform: none;
		letter-spacing: 0;
		font-weight: 600;
	}
	.live {
		color: var(--ok);
	}
	.copy {
		font-size: 11.5px;
		color: var(--accent);
		font-weight: 600;
		padding: 2px 6px;
		border-radius: 5px;
		text-transform: none;
		letter-spacing: 0;
	}
	.copy:hover {
		background: var(--bg-3);
	}
	.foot {
		border-top: 1px solid var(--line-soft);
		padding: var(--sp-2) var(--sp-4);
		display: flex;
		gap: var(--sp-4);
		font-size: 12px;
		color: var(--text-3);
		font-variant-numeric: tabular-nums;
	}
	.ok {
		color: var(--ok);
	}
	.bad {
		color: var(--warn);
	}
</style>

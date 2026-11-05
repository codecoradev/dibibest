<script lang="ts">
	import { onMount } from 'svelte';
	import Header from '../../lib/components/Header.svelte';
	import TextPane from '../../lib/components/TextPane.svelte';
	import Canvas from '../../lib/components/Canvas.svelte';
	import Sidebar from '../../lib/components/Sidebar.svelte';
	import { ws, type Layout } from '../../lib/workspace.svelte';
	import { auth } from '../../lib/auth.svelte';

	const ORDER: Layout[] = ['text', 'canvas', 'explorer'];
	const STATUS: Record<Layout, string> = {
		text: 'text-first layout · autosync on — text is the input, diagram is the preview',
		canvas: 'canvas-first layout · drag & edit on canvas → text regenerates live',
		explorer: 'explorer layout · search, hide and focus tables in large schemas'
	};

	// Phones get one pane at a time with a bottom tab bar (same state, same core).
	const PANES: Record<Layout, [string, string]> = {
		text: ['Editor', 'Diagram'],
		canvas: ['Diagram', 'Schema'],
		explorer: ['Tables', 'Diagram']
	};
	let mobile = $state(false);
	/** Which of the layout's two panes a phone shows: 0 = first, 1 = second. */
	let pane = $state<0 | 1>(0);
	// a new layout opens on its primary pane (the editor for text-first, the diagram otherwise)
	let lastLayout: Layout | undefined;
	$effect(() => {
		if (ws.layout !== lastLayout) {
			lastLayout = ws.layout;
			pane = 0;
		}
	});
	// explorer: picking a table on a phone jumps to the diagram, focused on it
	$effect(() => {
		if (mobile && ws.layout === 'explorer' && ws.focus) pane = 1;
	});

	onMount(() => {
		const mq = window.matchMedia('(max-width: 760px)');
		mobile = mq.matches;
		const onMq = (e: MediaQueryListEvent) => (mobile = e.matches);
		mq.addEventListener('change', onMq);
		ws.boot();
		auth.load();
		// best effort: flush a pending autosave when the tab is hidden or closed
		const flush = () => {
			if (ws.saveState === 'unsaved') ws.saveNow();
		};
		document.addEventListener('visibilitychange', flush);
		window.addEventListener('pagehide', flush);
		// ⌘/ (Ctrl+/) cycles the three layouts over the same ERD-JSON state.
		const onKey = (e: KeyboardEvent) => {
			if (e.key === '/' && (e.metaKey || e.ctrlKey)) {
				e.preventDefault();
				ws.layout = ORDER[(ORDER.indexOf(ws.layout) + 1) % ORDER.length];
			}
		};
		window.addEventListener('keydown', onKey);
		return () => {
			mq.removeEventListener('change', onMq);
			window.removeEventListener('keydown', onKey);
			document.removeEventListener('visibilitychange', flush);
			window.removeEventListener('pagehide', flush);
		};
	});
</script>

<svelte:head><title>dibibest — workspace</title></svelte:head>

<div class="app">
	<Header />
	<div class="main" class:mobile>
		{#if ws.layout === 'text'}
			{#if !mobile || pane === 0}<TextPane editable />{/if}
			{#if !mobile || pane === 1}<Canvas mode="preview" />{/if}
		{:else if ws.layout === 'canvas'}
			{#if !mobile || pane === 0}<Canvas mode="edit" />{/if}
			{#if !mobile || pane === 1}<TextPane editable={false} />{/if}
		{:else}
			{#if !mobile || pane === 0}<Sidebar />{/if}
			{#if !mobile || pane === 1}<Canvas mode="explorer" />{/if}
		{/if}
	</div>
	{#if mobile}
		<div class="panes" role="tablist" aria-label="Pane">
			{#each PANES[ws.layout] as label, i (label)}
				<button role="tab" aria-selected={pane === i} onclick={() => (pane = i as 0 | 1)}>{label}</button>
			{/each}
		</div>
	{:else}
		<footer class="statusbar">
			<span>{STATUS[ws.layout]}</span><span class="sep">·</span><span>⌘/ switch layout</span>
		</footer>
	{/if}
</div>

<style>
	.app {
		height: 100vh;
		height: 100dvh;
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}
	.main {
		flex: 1;
		display: flex;
		min-height: 0;
	}
	.statusbar {
		height: 28px;
		flex: 0 0 28px;
		background: var(--bg-1);
		border-top: 1px solid var(--line);
		display: flex;
		align-items: center;
		gap: var(--sp-4);
		padding: 0 var(--sp-4);
		font-size: 12px;
		color: var(--text-3);
	}
	.sep {
		color: var(--line);
	}
	/* one pane fills the screen on phones */
	.main.mobile > :global(*) {
		width: 100% !important;
		flex: 1 1 auto !important;
		border: none !important;
	}
	.panes {
		flex: 0 0 auto;
		display: flex;
		gap: var(--sp-2);
		padding: var(--sp-2) var(--sp-3) calc(var(--sp-2) + env(safe-area-inset-bottom));
		background: var(--bg-1);
		border-top: 1px solid var(--line);
	}
	.panes button {
		flex: 1;
		padding: 10px;
		border-radius: var(--r-md);
		color: var(--text-2);
		font-weight: 600;
	}
	.panes button[aria-selected='true'] {
		background: var(--accent-strong);
		color: #fff;
	}
</style>

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

	onMount(() => {
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
			window.removeEventListener('keydown', onKey);
			document.removeEventListener('visibilitychange', flush);
			window.removeEventListener('pagehide', flush);
		};
	});
</script>

<svelte:head><title>dibibest — workspace</title></svelte:head>

<div class="app">
	<Header />
	<div class="main">
		{#if ws.layout === 'text'}
			<TextPane editable />
			<Canvas mode="preview" />
		{:else if ws.layout === 'canvas'}
			<Canvas mode="edit" />
			<TextPane editable={false} />
		{:else}
			<Sidebar />
			<Canvas mode="explorer" />
		{/if}
	</div>
	<footer class="statusbar">
		<span>{STATUS[ws.layout]}</span><span class="sep">·</span><span>⌘/ switch layout</span>
	</footer>
</div>

<style>
	.app {
		height: 100vh;
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
</style>

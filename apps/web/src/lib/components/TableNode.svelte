<script lang="ts">
	import { Handle, Position, type NodeProps, type Node } from '@xyflow/svelte';
	import type { TableData } from '../core';
	import { ws } from '../workspace.svelte';

	let { data, selected }: NodeProps<Node<TableData, 'table'>> = $props();

	let editing = $state<string | null>(null);
	const finish = (from: string, e: { currentTarget: HTMLInputElement }) => {
		editing = null;
		ws.renameColumn(data.node.id, from, e.currentTarget.value.trim());
	};
	const collapsed = $derived(data.tone === 'dimmed');
</script>

<article class="erd-node {data.tone}" class:selected class:editable={data.editable} aria-label={data.node.name}>
	<header>
		<span class="tbl-icon">▦</span><h3>{data.node.name}</h3>
		<span class="badge">{data.node.columns.length}c</span>
	</header>
	<ul>
		{#each collapsed ? data.node.columns.filter((c) => c.isPk) : data.node.columns as c (c.name)}
			<li>
				<Handle type="target" position={Position.Left} id="{c.name}-t" />
				<span class="key" class:pk={c.isPk} class:fk={c.isFk}>{c.isPk ? 'PK' : c.isFk ? 'FK' : ''}</span>
				{#if editing === c.name}
					<input
						class="nodrag"
						value={c.name}
						aria-label="column name"
						onblur={(e) => finish(c.name, e)}
						onkeydown={(e) => e.key === 'Enter' && e.currentTarget.blur()}
					/>
				{:else}
					<span>{c.name}</span>
				{/if}
				<span class="ctype">{c.type}</span>
				{#if data.editable}
					<button class="edit nodrag" aria-label="rename {c.name}" onclick={() => (editing = c.name)}>✎</button>
				{/if}
				<Handle type="source" position={Position.Right} id="{c.name}-s" />
			</li>
		{/each}
	</ul>
</article>

<style>
	.erd-node {
		width: 220px;
		background: var(--bg-2);
		border: 1px solid var(--line);
		border-radius: var(--r-md);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
		font-size: 13px;
	}
	.erd-node.selected,
	.erd-node.related {
		border-color: var(--accent);
	}
	.erd-node.dimmed {
		opacity: 0.45;
	}
	header {
		padding: 8px var(--sp-3);
		border-bottom: 1px solid var(--line-soft);
		display: flex;
		align-items: center;
		gap: var(--sp-2);
	}
	.tbl-icon {
		color: var(--accent);
		font-size: 11px;
	}
	h3 {
		font-size: 12.5px;
		font-weight: 650;
	}
	.badge {
		margin-left: auto;
		color: var(--text-3);
		font-size: 11px;
		font-variant-numeric: tabular-nums;
	}
	ul {
		list-style: none;
		padding: var(--sp-1) 0;
	}
	li {
		position: relative;
		display: flex;
		align-items: center;
		gap: var(--sp-2);
		padding: 4px var(--sp-3);
		color: var(--text-2);
		font-family: var(--mono);
		font-size: 12px;
	}
	li:hover {
		background: var(--bg-3);
	}
	.key {
		width: 16px;
		text-align: center;
		font-size: 11px;
		font-weight: 700;
	}
	.key.pk {
		color: var(--pk);
	}
	.key.fk {
		color: var(--fk);
	}
	.ctype {
		margin-left: auto;
		color: var(--text-3);
		font-size: 11px;
	}
	.edit {
		color: var(--text-3);
		font-size: 11px;
		opacity: 0;
	}
	li:hover .edit,
	.edit:focus-visible {
		opacity: 1;
	}
	input {
		width: 90px;
		background: var(--bg-0);
		color: var(--text-1);
		border: 1px solid var(--accent);
		border-radius: 4px;
		font: inherit;
	}
	:global(.erd-node .svelte-flow__handle) {
		width: 6px;
		height: 6px;
		background: var(--fk);
		border: none;
		/* still the anchor of the edges, but only drawn where a drag can start from it */
		opacity: 0;
		transition: opacity 120ms;
	}
	:global(.erd-node.editable:hover .svelte-flow__handle),
	:global(.erd-node.selected .svelte-flow__handle) {
		opacity: 1;
	}
	@media (prefers-reduced-motion: reduce) {
		:global(.erd-node .svelte-flow__handle) {
			transition: none;
		}
	}
</style>

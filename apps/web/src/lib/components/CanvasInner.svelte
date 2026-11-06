<script lang="ts">
	import '@xyflow/svelte/dist/style.css';
	import {
		Background,
		BackgroundVariant,
		MarkerType,
		MiniMap,
		Panel,
		SvelteFlow,
		useSvelteFlow,
		type Connection,
		type Edge,
		type Node
	} from '@xyflow/svelte';
	import TableNode from './TableNode.svelte';
	import { ws } from '../workspace.svelte';
	import type { TableData } from '../core';

	let { mode }: { mode: 'preview' | 'edit' | 'explorer' } = $props();

	const nodeTypes = { table: TableNode };
	const { fitView } = useSvelteFlow();
	const edit = $derived(mode === 'edit');

	let nodes = $state.raw<Node[]>([]);
	let edges = $state.raw<Edge[]>([]);
	let viewport = $state.raw({ x: 0, y: 0, zoom: 1 });

	// Explorer focus: the focused table + its direct neighbours stay lit, the rest dim.
	const related = $derived.by(() => {
		if (mode !== 'explorer' || !ws.focus || !ws.erd) return null;
		const ids = new Set([ws.focus]);
		for (const e of ws.erd.edges) {
			if (e.from.table === ws.focus) ids.add(e.to.table);
			if (e.to.table === ws.focus) ids.add(e.from.table);
		}
		return ids;
	});

	// Store → xyflow. The store stays the only source of truth; xyflow state is a projection.
	$effect(() => {
		const erd = ws.erd;
		if (!erd) return;
		const shown = (id: string) => mode !== 'explorer' || !ws.hidden.has(id);
		nodes = erd.nodes
			.filter((n) => shown(n.id))
			.map((n) => {
				const tone = !related ? 'normal' : n.id === ws.focus ? 'normal' : related.has(n.id) ? 'related' : 'dimmed';
				return {
					id: n.id,
					type: 'table',
					position: n.position ?? { x: 0, y: 0 },
					data: { node: n, tone, editable: edit } satisfies TableData
				};
			});
		edges = erd.edges
			.filter((e) => shown(e.from.table) && shown(e.to.table))
			.map((e) => {
				const hot = !related || (related.has(e.from.table) && related.has(e.to.table));
				return {
					id: e.id,
					source: e.from.table,
					target: e.to.table,
					sourceHandle: `${e.from.columns[0]}-s`,
					targetHandle: `${e.to.columns[0]}-t`,
					// optional relationship (DBML `?`) = dashed, like dbdiagram
					style: `stroke: ${hot ? '#b48cff' : '#3a4250'}; stroke-width: 1.6${e.fromOptional || e.toOptional ? '; stroke-dasharray: 6 4' : ''}`,
					markerEnd: { type: MarkerType.ArrowClosed, color: hot ? '#b48cff' : '#3a4250' }
				};
			});
	});

	const col = (h: string | null | undefined) => (h ?? '').replace(/-[st]$/, '');
	const onconnect = (c: Connection) =>
		ws.addRelation(
			{ table: c.source, column: col(c.sourceHandle) },
			{ table: c.target, column: col(c.targetHandle) }
		);
	const ondragstop = () => ws.move(new Map(nodes.map((n) => [n.id, n.position])));
	const ondelete = ({ nodes: ns, edges: es }: { nodes: Node[]; edges: Edge[] }) =>
		ws.remove(
			ns.map((n) => n.id),
			es.map((e) => e.id)
		);

	const total = $derived(ws.erd?.nodes.length ?? 0);
</script>

<div class="canvas" aria-label="ERD canvas">
	<SvelteFlow
		bind:nodes
		bind:edges
		bind:viewport
		{nodeTypes}
		colorMode="dark"
		fitView
		fitViewOptions={{ maxZoom: 1 }}
		minZoom={0.1}
		nodesConnectable={edit}
		deleteKey={edit ? ['Backspace', 'Delete'] : null}
		onnodedragstop={ondragstop}
		{onconnect}
		{ondelete}
		onnodeclick={({ node }) => {
			if (mode === 'explorer') ws.focus = ws.focus === node.id ? null : node.id;
		}}
		onpaneclick={() => mode === 'explorer' && (ws.focus = null)}
		proOptions={{ hideAttribution: true }}
	>
		<Background variant={BackgroundVariant.Dots} gap={24} size={1} bgColor="var(--bg-0)" patternColor="#1a2029" />
		{#if mode === 'explorer'}<MiniMap pannable zoomable />{/if}

		<Panel position="top-left">
			<div class="toolbar" role="toolbar" aria-label="Canvas tools">
				<button onclick={() => fitView()}>⌖ Fit</button>
				<button onclick={() => ws.addTable()}>{edit ? '＋ Table' : 'Add table'}</button>
				{#if mode === 'explorer'}
					<button class:on={ws.focus} disabled={!ws.focus} onclick={() => (ws.focus = null)}>⊙ Focus mode</button>
				{/if}
			</div>
		</Panel>
		{#if mode === 'explorer'}
			<Panel position="top-right">
				<div class="toolbar layout-tools" role="toolbar" aria-label="Layout">
					<button class="on">Grid</button>
					<button disabled title="ELK layouts arrive in R4">Layered</button>
					<button disabled title="ELK layouts arrive in R4">Ortho</button>
				</div>
			</Panel>
		{/if}
		{#if mode === 'explorer' && ws.focus && related}
			<Panel position="top-center">
				<div class="focusbar">
					Focus: <b>{ws.focus}</b> — {related.size - 1} related, {total - related.size} dimmed
					<button aria-label="Clear focus" onclick={() => (ws.focus = null)}>✕</button>
				</div>
			</Panel>
		{/if}
		{#if edit}
			<Panel position="bottom-left">
				<div class="hint">drag column → another table to create a relation</div>
			</Panel>
		{:else if mode === 'preview'}
			<Panel position="bottom-left">
				<div class="hint solid">Live preview — <b>typing updates the diagram instantly</b></div>
			</Panel>
		{/if}
		<Panel position="bottom-right">
			<div class="hud">
				<span class="grp">{Math.round(viewport.zoom * 100)}%</span>
				<span class="grp">{total} tables · {ws.erd?.edges.length ?? 0} refs</span>
				{#if ws.error}<span class="grp warn">stale</span>{:else}<span class="grp ok">synced</span>{/if}
			</div>
		</Panel>
	</SvelteFlow>
</div>

<style>
	.canvas {
		flex: 1;
		min-width: 0;
		position: relative;
	}
	.canvas :global(.svelte-flow) {
		--xy-background-color: var(--bg-0);
		--xy-edge-stroke: #b48cff;
		--xy-minimap-background-color: var(--bg-1);
		--xy-minimap-mask-background-color: rgba(11, 13, 16, 0.7);
		--xy-minimap-node-background-color: var(--bg-3);
	}
	.canvas :global(.svelte-flow__node-table) {
		cursor: grab;
	}
	.toolbar,
	.focusbar,
	.hud .grp,
	.hint {
		background: var(--bg-2);
		border: 1px solid var(--line);
		border-radius: var(--r-md);
	}
	.toolbar {
		display: flex;
		gap: 2px;
		padding: 3px;
	}
	.toolbar button {
		padding: 6px 11px;
		border-radius: 7px;
		color: var(--text-2);
		font-size: 12.5px;
		font-weight: 550;
	}
	.toolbar button:hover:not(:disabled),
	.toolbar button.on {
		background: var(--bg-3);
		color: var(--text-1);
	}
	.toolbar button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
	.focusbar {
		padding: 6px var(--sp-3);
		font-size: 12.5px;
		color: var(--text-2);
		display: flex;
		gap: var(--sp-2);
		align-items: center;
	}
	.focusbar b {
		color: var(--text-1);
	}
	.hint {
		padding: 6px var(--sp-4);
		font-size: 12.5px;
		color: var(--text-2);
		border-style: dashed;
	}
	.hint.solid {
		border-style: solid;
	}
	.hint b {
		color: var(--text-1);
	}
	.hud {
		display: flex;
		gap: var(--sp-3);
	}
	.hud .grp {
		border-radius: var(--r-sm);
		padding: 5px 10px;
		font-family: var(--mono);
		font-size: 12px;
		color: var(--text-2);
		font-variant-numeric: tabular-nums;
	}
	.hud .grp.ok {
		color: var(--ok);
		border-color: rgba(62, 207, 142, 0.3);
	}
	.hud .grp.warn {
		color: var(--warn);
		border-color: rgba(245, 181, 68, 0.3);
	}
	/* Phones: keep the canvas clear; the tools stay, the hints and minimap go. */
	@media (max-width: 760px) {
		.hint,
		.hud .grp:not(.ok):not(.warn),
		.layout-tools,
		.canvas :global(.svelte-flow__minimap) {
			display: none;
		}
		.toolbar button {
			padding: 9px 12px;
		}
		.focusbar {
			max-width: calc(100vw - 24px);
		}
	}
</style>

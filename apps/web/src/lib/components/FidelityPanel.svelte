<script lang="ts">
	import { ws } from '../workspace.svelte';

	const f = $derived(ws.fidelity);
	const rows = $derived(
		f
			? ([
					['Tables', f.tables],
					['Columns', f.columns],
					['Refs', f.refs],
					['Indexes', f.indexes]
				] as const)
			: []
	);
</script>

{#if f}
	<aside class="panel" aria-label="Fidelity report">
		<h2>Fidelity report</h2>
		<table>
			<tbody>
				{#each rows as [name, o] (name)}
					<tr class:lost={o.kept !== o.total}>
						<th>{name}</th>
						<td>{o.kept} / {o.total}</td>
					</tr>
				{/each}
				<tr class:lost={!f.idempotent}>
					<th>Round-trip idempotent</th>
					<td>{f.idempotent ? 'yes' : 'no'}</td>
				</tr>
			</tbody>
		</table>
		{#if f.dropped?.length}
			<h3>Dropped</h3>
			<ul>
				{#each f.dropped as d, i (i)}<li>{d}</li>{/each}
			</ul>
		{/if}
	</aside>
{/if}

<style>
	.panel {
		position: absolute;
		top: calc(100% + 4px);
		right: var(--sp-4);
		max-width: calc(100vw - 24px);
		z-index: 20;
		width: 320px;
		background: var(--bg-2);
		border: 1px solid var(--line);
		border-radius: var(--r-md);
		padding: var(--sp-3) var(--sp-4);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
	}
	h2,
	h3 {
		font-size: 11.5px;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-3);
		margin: var(--sp-2) 0;
	}
	table {
		width: 100%;
		border-collapse: collapse;
		font-variant-numeric: tabular-nums;
	}
	th {
		text-align: left;
		font-weight: 500;
		color: var(--text-2);
		padding: 3px 0;
	}
	td {
		text-align: right;
		color: var(--ok);
		font-family: var(--mono);
		font-size: 12.5px;
	}
	tr.lost td {
		color: var(--warn);
	}
	ul {
		padding-left: var(--sp-4);
		color: var(--warn);
		font-size: 12.5px;
	}
</style>

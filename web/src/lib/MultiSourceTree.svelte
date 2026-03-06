<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import DirectoryTree from './DirectoryTree.svelte';

	export let sources: string[];
	export let activeSource: string | null;
	export let activePath: string | null;

	const dispatch = createEventDispatcher();

	let expanded: Record<string, boolean> = {};

	// Auto-expand the source that contains the active file.
	$: if (activeSource) expanded[activeSource] = true;
</script>

<div class="multi-tree">
	{#each sources as source (source)}
		<div class="source-root">
			<button
				class="source-header"
				class:active={source === activeSource}
				on:click={() => (expanded[source] = !expanded[source])}
			>
				{source}
			</button>
			{#if expanded[source]}
				<DirectoryTree
					{source}
					activePath={source === activeSource ? activePath : null}
					on:open={(e) => dispatch('open', e.detail)}
				/>
			{/if}
		</div>
	{/each}
</div>

<style>
	.multi-tree {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		background: var(--bg-secondary);
		border-right: 1px solid var(--border);
	}

	.source-root {
		display: flex;
		flex-direction: column;
		flex-shrink: 0;
	}

	.source-root:has(+ .source-root) {
		border-bottom: 1px solid var(--border);
	}

	.source-header {
		display: flex;
		align-items: center;
		padding: 6px 10px;
		background: none;
		border: none;
		cursor: pointer;
		font-size: 14px;
		font-weight: 500;
		color: var(--text-muted);
		text-align: left;
		width: 100%;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		border-left: 2px solid transparent;
	}

	.source-header:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.05));
		color: var(--text);
	}

	.source-header.active {
		color: var(--text);
		font-weight: 700;
		background: var(--bg-hover);
		border-left-color: var(--accent, #58a6ff);
	}

	/* DirectoryTree inside an expanded source: remove fixed height and border */
	.source-root :global(.tree) {
		border-right: none;
		height: auto;
	}
</style>

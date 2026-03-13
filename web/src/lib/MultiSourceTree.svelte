<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import DirectoryTree from './DirectoryTree.svelte';
	import { keyboardCursorPath } from '$lib/treeStore';

	export let sources: string[];
	export let activeSource: string | null;
	export let activePath: string | null;

	const dispatch = createEventDispatcher();

	let expanded: Record<string, boolean> = {};

	// Auto-expand the source that contains the active file.
	$: if (activeSource) expanded[activeSource] = true;

	// Track the last button that received focus inside the tree.
	// This is more reliable than document.activeElement, which can be stale
	// if focus changes between the keydown firing and the handler running.
	let lastFocused: HTMLElement | null = null;

	function handleFocusin(e: FocusEvent) {
		const t = e.target as HTMLElement;
		if (t.dataset.treeNav) lastFocused = t;
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
		const container = e.currentTarget as HTMLElement;
		const items = Array.from(container.querySelectorAll<HTMLElement>('[data-tree-nav]'));
		const active = document.activeElement as HTMLElement;
		// Prefer document.activeElement; fall back to the last element that had focus.
		let idx = items.indexOf(active);
		if (idx === -1 && lastFocused) idx = items.indexOf(lastFocused);
		if (idx === -1) return;
		e.preventDefault();
		const next = e.key === 'ArrowDown' ? idx + 1 : idx - 1;
		if (next >= 0 && next < items.length) {
			const target = items[next];
			lastFocused = target;
			keyboardCursorPath.set(target.dataset.treePath ?? null);
			target.focus();
		}
	}
</script>

<div class="multi-tree" role="tree" tabindex="-1" on:focusin={handleFocusin} on:keydown={handleKeydown}>
	{#each sources as source (source)}
		<div class="source-root">
			<button
				class="source-header"
				class:active={source === activeSource}
				data-tree-nav="source"
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

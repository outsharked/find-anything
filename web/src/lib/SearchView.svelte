<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { goto } from '$app/navigation';
	import SearchBox from '$lib/SearchBox.svelte';
	import SourceSelector from '$lib/SourceSelector.svelte';
	import ResultList from '$lib/ResultList.svelte';
	import type { SearchResult } from '$lib/api';

	export let query: string;
	export let mode: string;
	export let searching: boolean;
	export let sources: string[];
	export let selectedSources: string[];
	export let results: SearchResult[] = [];
	export let totalResults = 0;
	export let searchError: string | null = null;
	export let searchId = 0;
	export let showTree = false;

	const dispatch = createEventDispatcher<{
		search: { query: string; mode: string };
		sourceChange: string[];
		open: SearchResult;
		treeToggle: void;
	}>();

	let isTyping = false;
	$: isSearchActive = isTyping || searching;
</script>

<div class="topbar">
	<span class="logo">find-anything</span>
	<button
		class="tree-toggle"
		class:active={showTree}
		title="Toggle file tree"
		on:click={() => dispatch('treeToggle')}
	>⊞</button>
	<div class="search-wrap">
		<SearchBox
			{query}
			{mode}
			searching={isSearchActive}
			bind:isTyping
			on:change={(e) => dispatch('search', e.detail)}
		/>
	</div>
	{#if sources.length > 0}
		<SourceSelector
			{sources}
			selected={selectedSources}
			on:change={(e) => dispatch('sourceChange', e.detail)}
		/>
	{/if}
	<button class="gear-btn" title="Settings" on:click={() => goto('/settings')}>⚙</button>
</div>

<div class="content">
	{#if searchError}
		<div class="status error">{searchError}</div>
	{:else if query.trim().length >= 3}
		{#if !isSearchActive || totalResults > 0}
			<div class="result-meta">
				{totalResults.toLocaleString()} result{totalResults !== 1 ? 's' : ''}
			</div>
		{/if}
		{#key searchId}
			<ResultList
				{results}
				searching={isSearchActive}
				on:open={(e) => dispatch('open', e.detail)}
			/>
		{/key}
	{/if}
</div>

<style>
	.topbar {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 16px;
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
		position: sticky;
		top: 0;
		z-index: 100;
	}

	.logo {
		font-size: 14px;
		font-weight: 600;
		color: var(--text);
		white-space: nowrap;
		flex-shrink: 0;
	}

	.search-wrap {
		min-width: 260px;
		flex: 1;
	}

	.tree-toggle {
		background: none;
		border: none;
		cursor: pointer;
		color: var(--text-muted);
		font-size: 16px;
		padding: 2px 6px;
		border-radius: 4px;
		line-height: 1;
		flex-shrink: 0;
	}

	.tree-toggle:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
		color: var(--text);
	}

	.tree-toggle.active {
		color: var(--accent, #58a6ff);
	}

	.gear-btn {
		background: none;
		border: none;
		cursor: pointer;
		color: var(--text-muted);
		font-size: 20px;
		padding: 2px 6px;
		border-radius: 4px;
		line-height: 1;
		flex-shrink: 0;
	}

	.gear-btn:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
		color: var(--text);
	}

	.content {
		padding: 0 16px;
		width: 100%;
	}

	.status {
		padding: 24px;
		color: var(--text-muted);
		text-align: center;
	}

	.status.error {
		color: #f85149;
	}

	.result-meta {
		padding: 12px 0 4px;
		color: var(--text-muted);
		font-size: 13px;
	}
</style>

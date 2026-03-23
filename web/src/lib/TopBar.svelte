<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { goto } from '$app/navigation';
	import SearchBox from '$lib/SearchBox.svelte';
	import AdvancedSearch from '$lib/AdvancedSearch.svelte';
	import SearchHelp from '$lib/SearchHelp.svelte';
	import AppLogo from '$lib/AppLogo.svelte';
	import MobilePanel from '$lib/MobilePanel.svelte';
	import SearchHelpContent from '$lib/SearchHelpContent.svelte';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';

	export let query: string;
	export let searching: boolean;
	export let showTree: boolean;
	export let sources: string[] = [];
	export let selectedSources: string[] = [];
	export let selectedKinds: string[] = [];
	export let dateFrom = '';
	export let dateTo = '';
	export let caseSensitive = false;
	export let scope: SearchScope = 'line';
	export let matchType: SearchMatchType = 'fuzzy';
	export let nlpDetectedPhrase: string | undefined = undefined;

	const dispatch = createEventDispatcher<{
		search: { query: string };
		treeToggle: void;
		filterChange: { sources: string[]; kinds: string[]; dateFrom?: number; dateTo?: number; caseSensitive: boolean; scope: SearchScope; matchType: SearchMatchType };
	}>();

	export let isSearchActive = false;

	let helpOpen = false;
	let isTyping = false;
	$: isSearchActive = isTyping || searching;
	$: nlpHighlightSpan = (() => {
		if (!nlpDetectedPhrase || isTyping) return undefined;
		const idx = query.toLowerCase().indexOf(nlpDetectedPhrase.toLowerCase());
		if (idx === -1) return undefined;
		return [idx, idx + nlpDetectedPhrase.length] as [number, number];
	})();

	let searchBox: SearchBox;
	export function focus() { searchBox?.focus(); }
</script>

<div class="topbar">
	<button class="logo-btn" on:click={() => (helpOpen = true)} aria-label="Search help">
		<AppLogo />
	</button>
	<button
		class="tree-toggle"
		class:active={showTree}
		data-tooltip="Toggle file tree"
		on:click={() => dispatch('treeToggle')}
	>◫</button>
	<div class="help-wrap-outer"><SearchHelp bind:open={helpOpen} /></div>
	<div class="search-wrap">
		<SearchBox
			bind:this={searchBox}
			{query}
			searching={isSearchActive}
			{nlpHighlightSpan}
			bind:isTyping
			on:change={(e) => dispatch('search', { query: e.detail.query })}
		/>
	</div>
	{#if sources.length > 0}
		<div class="advanced-wrap">
			<AdvancedSearch
				{sources}
				{selectedSources}
				{selectedKinds}
				{dateFrom}
				{dateTo}
				{caseSensitive}
				{scope}
				{matchType}
				on:change={(e) => dispatch('filterChange', e.detail)}
			/>
		</div>
	{/if}
	<button class="gear-btn" on:click={() => goto('/settings')}>⚙</button>
</div>

<MobilePanel bind:open={helpOpen} title="Search Help">
	<SearchHelpContent />
</MobilePanel>

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

	.logo-btn {
		background: none;
		border: none;
		padding: 0;
		cursor: default;
		flex-shrink: 0;
	}

	.advanced-wrap { display: contents; }

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
		position: relative;
	}

	.tree-toggle:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
		color: var(--text);
	}

	.tree-toggle.active { color: var(--accent, #58a6ff); }

	.tree-toggle[data-tooltip]::after {
		content: attr(data-tooltip);
		position: absolute;
		top: calc(100% + 4px);
		left: 50%;
		transform: translateX(-50%);
		white-space: nowrap;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		color: var(--text-muted);
		padding: 2px 6px;
		border-radius: 3px;
		font-size: 11px;
		opacity: 0;
		pointer-events: none;
		transition: opacity 0.1s;
		z-index: 100;
	}

	.tree-toggle[data-tooltip]:hover::after { opacity: 1; }

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

	@media (max-width: 768px) {
		.topbar { gap: 6px; padding: 6px 10px; }
		.tree-toggle { display: none; }
		.help-wrap-outer { display: none; }
		.logo-btn { cursor: pointer; order: 1; }
		.search-wrap { order: 2; flex: 1 1 0; min-width: 0; }
		.advanced-wrap { order: 3; display: block; }
		.gear-btn { order: 4; min-width: 36px; min-height: 36px; display: flex; align-items: center; justify-content: center; }
	}
</style>

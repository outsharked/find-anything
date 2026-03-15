<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { goto } from '$app/navigation';
	import SearchBox from '$lib/SearchBox.svelte';
	import AdvancedSearch from '$lib/AdvancedSearch.svelte';
	import ResultList from '$lib/ResultList.svelte';
	import type { SearchResult } from '$lib/api';
	import { parseSearchPrefixes } from '$lib/searchPrefixes';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';

	export let query: string;
	export let scope: SearchScope = 'line';
	export let matchType: SearchMatchType = 'fuzzy';
	export let searching: boolean;
	export let sources: string[];
	export let selectedSources: string[];
	export let selectedKinds: string[] = [];
	export let dateFrom = '';
	export let dateTo = '';
	export let caseSensitive = false;
	export let results: SearchResult[] = [];
	export let totalResults = 0;
	export let searchError: string | null = null;
	export let searchId = 0;
	export let showTree = false;
	export let filterDateFrom: number | undefined = undefined;
	export let filterDateTo: number | undefined = undefined;
	export let nlpDateLabel: string | undefined = undefined;
	export let nlpDetectedPhrase: string | undefined = undefined;
	export let nlpConflict = false;
	export let resultsStale = false;
	export let deletedPaths: Set<string> = new Set();

	const dispatch = createEventDispatcher<{
		search: { query: string };
		filterChange: { sources: string[]; kinds: string[]; dateFrom?: number; dateTo?: number; caseSensitive: boolean; scope: SearchScope; matchType: SearchMatchType };
		clearNlpDate: void;
		open: SearchResult;
		treeToggle: void;
		refreshResults: void;
		dismissStale: void;
	}>();

	let searchBox: SearchBox;
	export function focus() { searchBox?.focus(); }

	let isTyping = false;
	$: isSearchActive = isTyping || searching;

	// Compute prefix chips from the current query.
	$: prefixResult = parseSearchPrefixes(query);
	$: prefixTokens = prefixResult.prefixTokens;

	function removePrefixToken(token: { raw: string; value: string }) {
		// Replace the full prefix token with its bare value (the non-prefix part),
		// so e.g. "file:extra" → "extra" rather than removing "extra" too.
		const parts = query.split(/\s+/);
		const newQuery = parts
			.flatMap((t) => (t === token.raw ? (token.value ? [token.value] : []) : [t]))
			.join(' ');
		dispatch('search', { query: newQuery });
	}

	// Find the detected date phrase in the current query for inline highlighting.
	// Returns undefined when typing (stale span) or when phrase no longer matches.
	$: nlpHighlightSpan = (() => {
		if (!nlpDetectedPhrase || isTyping) return undefined;
		const idx = query.toLowerCase().indexOf(nlpDetectedPhrase.toLowerCase());
		if (idx === -1) return undefined;
		return [idx, idx + nlpDetectedPhrase.length] as [number, number];
	})();

	const SHORT_DATE = new Intl.DateTimeFormat('en-US', { month: 'numeric', day: 'numeric', year: 'numeric' });
	function fmtTs(ts: number): string { return SHORT_DATE.format(new Date(ts * 1000)); }
	$: resultDateSuffix = (() => {
		if (filterDateFrom != null && filterDateTo != null) return ` between ${fmtTs(filterDateFrom)} and ${fmtTs(filterDateTo)}`;
		if (filterDateFrom != null) return ` after ${fmtTs(filterDateFrom)}`;
		if (filterDateTo != null) return ` before ${fmtTs(filterDateTo)}`;
		return '';
	})();
</script>

<div class="topbar">
	<span class="logo">find-anything</span>
	<button
		class="tree-toggle"
		class:active={showTree}
		title="Toggle file tree"
		on:click={() => dispatch('treeToggle')}
	>◫</button>
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
	{/if}
	<button class="gear-btn" title="Settings" on:click={() => goto('/settings')}>⚙</button>
</div>

{#if prefixTokens.length > 0}
	<div class="nlp-bar prefix-bar">
		{#each prefixTokens as token (token.raw)}
			<div class="nlp-chip prefix-chip">
				<span class="nlp-label">{[
					token.scope === 'file' ? 'filename' : token.scope === 'doc' ? 'document' : null,
					token.match,
					token.kind ? `type: ${token.kind}` : null,
				].filter(Boolean).join(' · ')}</span>
				<button class="nlp-dismiss" on:click={() => removePrefixToken(token)} aria-label="Remove prefix">✕</button>
			</div>
		{/each}
	</div>
{/if}

{#if nlpDateLabel}
	<div class="nlp-bar">
		<div class="nlp-chip" class:conflict={nlpConflict}>
			<span class="nlp-label">Filtered: {nlpDateLabel}</span>
			{#if nlpConflict}
				<span
					class="conflict-icon"
					title={`A date was detected in your query ("${nlpDetectedPhrase}") but a manual date range is also set in Advanced search. The manual range takes precedence — clear the Advanced date range to use the query date instead.`}
					aria-label="Date conflict: manual range overrides query date"
				>!</span>
			{:else}
				<button class="nlp-dismiss" on:click={() => dispatch('clearNlpDate')} aria-label="Clear detected date">✕</button>
			{/if}
		</div>
	</div>
{/if}

<div class="content">
	{#if searchError}
		<div class="status error">{searchError}</div>
	{:else if query.trim().length >= 3}
		{#if !isSearchActive || totalResults > 0}
			<div class="result-meta">
				{totalResults.toLocaleString()} result{totalResults !== 1 ? 's' : ''}{resultDateSuffix}
			</div>
		{/if}
		{#if resultsStale}
			<div class="stale-banner">
				Index updated —
				<button class="stale-refresh" on:click={() => dispatch('refreshResults')}>refresh results</button>
				<button class="stale-dismiss" on:click={() => dispatch('dismissStale')} aria-label="Dismiss">✕</button>
			</div>
		{/if}
		{#key searchId}
			<ResultList
				{results}
				searching={isSearchActive}
				{deletedPaths}
				{query}
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

	.nlp-bar {
		padding: 6px 16px 0;
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 6px;
	}

	.prefix-bar {
		flex-wrap: wrap;
	}

	.nlp-chip {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 3px 8px;
		border-radius: 20px;
		background: var(--chip-bg);
		border: 1px solid var(--border);
		font-size: 12px;
		color: var(--text-muted);
	}

	.nlp-chip.conflict {
		opacity: 0.6;
	}

	.nlp-chip.conflict .nlp-label {
		text-decoration: line-through;
	}

	.nlp-dismiss {
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		color: var(--text-dim);
		font-size: 11px;
		line-height: 1;
		display: flex;
		align-items: center;
	}

	.nlp-dismiss:hover {
		color: var(--text);
	}

	.conflict-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: #da3633;
		color: #fff;
		font-size: 10px;
		font-weight: 700;
		cursor: help;
		flex-shrink: 0;
	}

	.stale-banner {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 6px 16px;
		background: rgba(230, 162, 60, 0.1);
		border-bottom: 1px solid rgba(230, 162, 60, 0.25);
		color: #e6a23c;
		font-size: 12px;
		flex-shrink: 0;
	}

	.stale-refresh {
		background: none;
		border: none;
		padding: 0;
		font: inherit;
		font-size: 12px;
		color: inherit;
		cursor: pointer;
		text-decoration: underline;
	}

	.stale-dismiss {
		background: none;
		border: none;
		padding: 0;
		font-size: 12px;
		color: inherit;
		opacity: 0.6;
		cursor: pointer;
		margin-left: auto;
	}

	.stale-dismiss:hover {
		opacity: 1;
	}
</style>

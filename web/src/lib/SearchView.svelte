<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import ResultList from '$lib/ResultList.svelte';
	import type { SearchResult } from '$lib/api';
	import { parseSearchPrefixes } from '$lib/searchPrefixes';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';
	import TopBar from '$lib/TopBar.svelte';

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
	export let resultsCapped = false;
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

	let topBar: TopBar;
	let isSearchActive = false;
	export function focus() { topBar?.focus(); }

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

	const SHORT_DATE = new Intl.DateTimeFormat('en-US', { month: 'numeric', day: 'numeric', year: 'numeric' });
	function fmtTs(ts: number): string { return SHORT_DATE.format(new Date(ts * 1000)); }
	$: resultDateSuffix = (() => {
		if (filterDateFrom != null && filterDateTo != null) return ` between ${fmtTs(filterDateFrom)} and ${fmtTs(filterDateTo)}`;
		if (filterDateFrom != null) return ` after ${fmtTs(filterDateFrom)}`;
		if (filterDateTo != null) return ` before ${fmtTs(filterDateTo)}`;
		return '';
	})();
</script>

<TopBar
	bind:this={topBar}
	bind:isSearchActive
	{query}
	{searching}
	{showTree}
	{sources}
	{selectedSources}
	{selectedKinds}
	{dateFrom}
	{dateTo}
	{caseSensitive}
	{scope}
	{matchType}
	{nlpDetectedPhrase}
	on:search={(e) => dispatch('search', e.detail)}
	on:treeToggle={() => dispatch('treeToggle')}
	on:filterChange={(e) => dispatch('filterChange', e.detail)}
/>

{#if prefixTokens.length > 0 || nlpDateLabel}
	<div class="nlp-bar">
		{#each prefixTokens as token (token.raw)}
			<div class="nlp-chip prefix-chip">
				<span class="nlp-label">{[
					token.scope === 'file' ? 'filename' : token.scope === 'doc' ? 'document' : null,
					token.match,
					token.kind ? `type: ${token.kind}` : null,
					token.dirSource ? `source: ${token.dirSource}${token.dirPrefix ? '/' + token.dirPrefix : ''}` : null,
				].filter(Boolean).join(' · ')}</span>
				<button class="nlp-dismiss" on:click={() => removePrefixToken(token)} aria-label="Remove prefix">✕</button>
			</div>
		{/each}
		{#if nlpDateLabel}
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
		{/if}
	</div>
{/if}

<div class="content">
	{#if searchError}
		<div class="status error">{searchError}</div>
	{:else if query.trim().length >= 3}
		{#if !isSearchActive || totalResults > 0}
			<div class="result-meta">
				{totalResults.toLocaleString()}{resultsCapped ? '+' : ''} result{totalResults !== 1 ? 's' : ''}{resultDateSuffix}
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
		flex-wrap: nowrap;
		gap: 6px;
		overflow: hidden;
		flex-shrink: 0;
	}

.nlp-chip {
		display: inline-flex;
		align-items: center;
		flex-shrink: 0;
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

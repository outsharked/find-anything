<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { SearchResult } from '$lib/api';
	import SearchResultItem from '$lib/SearchResult.svelte';

	export let results: SearchResult[] = [];
	export let searching = false;

	const dispatch = createEventDispatcher<{ open: SearchResult }>();

	// Group results by file so multiple hits in the same file appear as one card.
	type ResultGroup = { key: string; hits: SearchResult[] };

	$: groups = (() => {
		const map = new Map<string, ResultGroup>();
		const order: string[] = [];
		for (const r of results) {
			const key = `${r.source}:${r.path}:${r.archive_path ?? ''}`;
			if (!map.has(key)) {
				map.set(key, { key, hits: [] });
				order.push(key);
			}
			map.get(key)!.hits.push(r);
		}
		return order.map((k) => map.get(k)!);
	})();
</script>

<div class="result-list" class:searching>
	{#if groups.length === 0 && !searching}
		<p class="empty">No results.</p>
	{:else}
		{#each groups as group (group.key)}
			<div class="result-pad">
				<SearchResultItem hits={group.hits} on:open={(e) => dispatch('open', e.detail)} />
			</div>
		{/each}
	{/if}
</div>

<style>
	.result-list {
		transition: opacity 0.2s ease-in-out;
	}

	.result-list.searching {
		opacity: 0.5;
		filter: blur(2px);
		pointer-events: none;
	}

	.result-pad {
		padding: 6px 0 0;
	}

	.result-pad:last-child {
		padding-bottom: 6px;
	}

	.empty {
		color: var(--text-muted);
		padding: 24px;
		text-align: center;
	}
</style>

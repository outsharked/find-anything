<script lang="ts">
	import { createEventDispatcher, beforeUpdate, tick } from 'svelte';
	import { listFiles } from '$lib/api';
	import type { FileRecord } from '$lib/api';
	import { splitDisplayPath, archivePathOf } from '$lib/commandPaletteLogic';

	/** Set to true to show the palette. */
	export let open = false;
	/** Source(s) to search. Empty = no filter (all sources). */
	export let sources: string[] = [];
	/** Total number of sources available — used to decide whether to show "all". */
	export let totalSourceCount = 0;

	const dispatch = createEventDispatcher<{
		select: { source: string; path: string; archivePath: string | null; kind: string };
		close: void;
	}>();

	type SourcedFile = FileRecord & { source: string };

	let query = '';
	let selected = 0;
	let inputEl: HTMLInputElement;
	let results: SourcedFile[] = [];
	let loading = false;
	let debounceTimer: ReturnType<typeof setTimeout> | null = null;

	$: isAll = totalSourceCount > 1 && sources.length >= totalSourceCount;

	// Use beforeUpdate + previous-value guard to react to open transitioning
	// false→true. A `$: if (open)` reactive block reads `inputEl` (inside its
	// tick callback), so bind:this re-triggers it every flush — infinite loop.
	let prevOpen = false;
	beforeUpdate(() => {
		if (open && !prevOpen) {
			query = '';
			results = [];
			loading = true;
			tick().then(() => {
				inputEl?.focus();
				fetchResults('');
			});
		}
		prevOpen = open;
	});

	function handleInput(e: Event) {
		query = (e.target as HTMLInputElement).value;
		scheduleSearch(query);
	}

	function scheduleSearch(q: string) {
		if (debounceTimer) clearTimeout(debounceTimer);
		debounceTimer = setTimeout(() => fetchResults(q), 500);
	}

	async function fetchResults(q: string) {
		if (!sources.length) return;
		loading = true;
		try {
			const all = await Promise.all(
				sources.map(async (src) => {
					const records = await listFiles(src, q, 50);
					return records.map((r): SourcedFile => ({ ...r, source: src }));
				})
			);
			results = all.flat();
			selected = 0;
		} catch {
			// partial failures silently yield no items for that source
		} finally {
			loading = false;
		}
	}

	function scrollSelected() {
		tick().then(() => {
			document.querySelector('.cp-item.active')?.scrollIntoView({ block: 'nearest' });
		});
	}

	function close() {
		dispatch('close');
	}

	function confirm() {
		const item = results[selected];
		if (item) {
			const i = item.path.indexOf('::');
			const outerPath = i >= 0 ? item.path.slice(0, i) : item.path;
			const archivePath = i >= 0 ? item.path.slice(i + 2) : null;
			dispatch('select', { source: item.source, path: outerPath, archivePath, kind: item.kind });
			close();
		}
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			close();
		} else if (e.key === 'ArrowDown') {
			e.preventDefault();
			selected = Math.min(selected + 1, results.length - 1);
			scrollSelected();
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			selected = Math.max(selected - 1, 0);
			scrollSelected();
		} else if (e.key === 'Enter') {
			confirm();
		}
	}
</script>

{#if open}
	<!-- svelte-ignore a11y-no-static-element-interactions -->
	<div class="cp-backdrop" on:click={close} on:keydown={onKeydown}>
		<!-- svelte-ignore a11y-no-static-element-interactions -->
		<div class="cp-panel" on:click|stopPropagation on:keydown|stopPropagation>
			<div class="cp-input-wrap">
				<span class="cp-icon">⌕</span>
				{#if isAll}
					<span class="cp-scope cp-scope-all">all</span>
				{:else}
					{#each sources as src}
						<span class="cp-scope">{src}</span>
					{/each}
				{/if}
				<input
					bind:this={inputEl}
					bind:value={query}
					class="cp-input"
					placeholder="Go to file…"
					autocomplete="off"
					spellcheck="false"
					on:input={handleInput}
					on:keydown={onKeydown}
				/>
			</div>
			<div class="cp-results">
				{#if results.length === 0 && loading}
					<div class="cp-status">Loading…</div>
				{:else if results.length === 0}
					<div class="cp-status">{query ? 'No matches' : 'No files indexed'}</div>
				{:else}
					{#each results as item, i (`${item.source}:${item.path}`)}
						<button
							type="button"
							class="cp-item"
							class:active={i === selected}
							on:click={confirm}
							on:mouseenter={() => (selected = i)}
						>
							<span class="cp-name">{splitDisplayPath(item.path).name}</span>
							{#if splitDisplayPath(item.path).dir}
								<span class="cp-dir">{splitDisplayPath(item.path).dir}</span>
							{/if}
							{#if sources.length > 1}
								<span class="cp-source">{item.source}</span>
							{/if}
						</button>
					{/each}
				{/if}
			</div>
		</div>
	</div>
{/if}

<style>
	.cp-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.5);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding-top: 15vh;
		z-index: 1000;
	}

	.cp-panel {
		width: min(800px, 90vw);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 8px;
		overflow: hidden;
		box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
	}

	.cp-input-wrap {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 10px 14px;
		border-bottom: 1px solid var(--border);
	}

	.cp-icon {
		color: var(--text-muted);
		font-size: 16px;
		flex-shrink: 0;
	}

	.cp-input {
		flex: 1;
		background: none;
		border: none;
		outline: none;
		color: var(--text);
		font-size: 14px;
		font-family: var(--font-mono);
	}

	.cp-scope {
		font-size: 11px;
		color: var(--text-muted);
		background: var(--badge-bg);
		border: 1px solid var(--border);
		padding: 1px 8px;
		border-radius: 20px;
		flex-shrink: 0;
		white-space: nowrap;
	}

	.cp-scope-all {
		opacity: 0.7;
	}

	.cp-source {
		font-size: 11px;
		color: var(--text-muted);
		background: var(--badge-bg);
		padding: 1px 8px;
		border-radius: 20px;
		flex-shrink: 0;
	}

	.cp-results {
		max-height: 360px;
		overflow-y: auto;
		overflow-x: hidden;
	}

	.cp-status {
		padding: 16px;
		color: var(--text-muted);
		font-size: 13px;
		text-align: center;
	}

	.cp-item {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		background: none;
		border: none;
		text-align: left;
		padding: 6px 14px;
		cursor: pointer;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		white-space: nowrap;
		overflow: hidden;
		box-sizing: border-box;
	}

	.cp-item:hover,
	.cp-item.active {
		background: var(--bg-hover);
		color: var(--text);
	}

	.cp-item.active {
		background: var(--accent-subtle, rgba(88, 166, 255, 0.15));
		color: var(--accent, #58a6ff);
	}

	.cp-name {
		flex-shrink: 0;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.cp-dir {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		color: var(--text-muted);
		font-size: 11px;
		padding-left: 10px;
		white-space: nowrap;
		opacity: 0.65;
	}
</style>

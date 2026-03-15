<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';
	import { listFiles } from '$lib/api';
	import type { FileRecord } from '$lib/api';
	import { buildItems, filterItems, splitDisplayPath, archivePathOf } from '$lib/commandPaletteLogic';

	/** Set to true to show the palette. */
	export let open = false;
	/** Source(s) to search. Empty = no filter (all sources). */
	export let sources: string[] = [];

	const dispatch = createEventDispatcher<{
		select: { source: string; path: string; archivePath: string | null; kind: string };
		close: void;
	}>();

	let query = '';
	let selected = 0;
	let inputEl: HTMLInputElement;

	// Per-source file list cache. Must be `let` (not `const`) so Svelte's
	// reactivity tracks it — Map mutations don't trigger updates, but
	// reassigning `cache = cache` after each set does.
	let cache = new Map<string, FileRecord[]>();

	let loading = false;

	// Fetch all scoped sources in parallel when palette opens or sources change.
	$: if (open && sources.length) loadAll(sources);

	async function ensureLoaded(source: string): Promise<void> {
		if (cache.has(source)) return;
		const records = await listFiles(source);
		cache.set(source, records);
		cache = cache; // trigger Svelte reactivity for allItems
	}

	async function loadAll(srcs: string[]) {
		loading = true;
		try {
			await Promise.all(srcs.map(ensureLoaded));
		} catch {
			// partial failures are silently swallowed; missing sources yield no items
		} finally {
			loading = false;
		}
	}

	$: allItems = buildItems(cache, sources);
	$: filtered = filterItems(allItems, query);

	$: if (filtered) selected = 0;

	$: if (open) tick().then(() => inputEl?.focus());

	function close() {
		query = '';
		dispatch('close');
	}

	function confirm() {
		const item = filtered[selected];
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
			selected = Math.min(selected + 1, filtered.length - 1);
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			selected = Math.max(selected - 1, 0);
		} else if (e.key === 'Enter') {
			confirm();
		}
	}

	$: if (typeof document !== 'undefined' && selected >= 0) {
		tick().then(() => {
			document.querySelector('.cp-item.active')?.scrollIntoView({ block: 'nearest' });
		});
	}
</script>

{#if open}
	<!-- svelte-ignore a11y-no-static-element-interactions -->
	<div class="cp-backdrop" on:click={close} on:keydown={onKeydown}>
		<!-- svelte-ignore a11y-no-static-element-interactions -->
		<div class="cp-panel" on:click|stopPropagation on:keydown|stopPropagation>
			<div class="cp-input-wrap">
				<span class="cp-icon">⌕</span>
				<input
					bind:this={inputEl}
					bind:value={query}
					class="cp-input"
					placeholder="Go to file…"
					on:keydown={onKeydown}
				/>
			</div>
			<div class="cp-results">
				{#if loading}
					<div class="cp-status">Loading files…</div>
				{:else if filtered.length === 0}
					<div class="cp-status">No matches</div>
				{:else}
					{#each filtered as item, i (`${item.source}:${item.path}`)}
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

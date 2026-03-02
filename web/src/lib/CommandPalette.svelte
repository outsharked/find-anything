<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';
	import { listFiles } from '$lib/api';
	import type { FileRecord } from '$lib/api';

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

	// Per-source file list cache.
	const cache = new Map<string, FileRecord[]>();

	type SourcedFile = FileRecord & { source: string };

	let loading = false;

	// Fetch all scoped sources in parallel when palette opens or sources change.
	$: if (open && sources.length) loadAll(sources);

	async function ensureLoaded(source: string): Promise<void> {
		if (cache.has(source)) return;
		const records = await listFiles(source);
		cache.set(source, records);
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

	$: allItems = sources.flatMap((s) =>
		(cache.get(s) ?? []).map((f): SourcedFile => ({ ...f, source: s }))
	);

	// Simple character-subsequence fuzzy scorer with exact match boosting.
	function fuzzyScore(q: string, path: string): number {
		if (!q) return 0;
		const ql = q.toLowerCase();
		const pl = path.toLowerCase();

		// Huge bonus for exact substring match (case-insensitive)
		if (pl.includes(ql)) {
			let bonus = 100;
			// Extra bonus if match is in the filename portion (after last / or ::)
			const lastSlash = Math.max(pl.lastIndexOf('/'), pl.lastIndexOf('::'));
			const filename = lastSlash >= 0 ? pl.slice(lastSlash + 1) : pl;
			if (filename.includes(ql)) bonus += 50;
			// Extra bonus if match is at the start of filename
			if (filename.startsWith(ql)) bonus += 50;
			return bonus;
		}

		// Fallback to character subsequence matching
		let qi = 0;
		let score = 0;
		let lastMatch = -1;
		for (let pi = 0; pi < pl.length && qi < ql.length; pi++) {
			if (pl[pi] === ql[qi]) {
				if (pi === lastMatch + 1) score += 2;
				if (pi === 0 || '/-_.'.includes(pl[pi - 1])) score += 3;
				lastMatch = pi;
				qi++;
			}
		}
		return qi === ql.length ? score : -1;
	}

	/** For a composite path "archive.zip::member.txt", returns the member portion. */
	function archivePathOf(path: string): string | null {
		const i = path.indexOf('::');
		return i >= 0 ? path.slice(i + 2) : null;
	}

	/** Display label for a file record: show archive members as "zip → member". */
	function displayPath(path: string): string {
		const i = path.indexOf('::');
		if (i < 0) return path;
		const zip = path.slice(0, i);
		const member = path.slice(i + 2);
		return `${zip} → ${member}`;
	}

	$: filtered = (() => {
		if (!query) return allItems.slice(0, 50).map((f) => ({ ...f, score: 0 }));
		return allItems
			.map((f) => ({ ...f, score: fuzzyScore(query, f.path) }))
			.filter((f) => f.score >= 0)
			.sort((a, b) => b.score - a.score)
			.slice(0, 50);
	})();

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
							<span class="cp-path">{displayPath(item.path)}</span>
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
		width: min(640px, 90vw);
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

	.cp-path {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
	}
</style>

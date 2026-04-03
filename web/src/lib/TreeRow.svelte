<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';
	import { listDir, listArchiveMembers } from '$lib/api';
	import type { DirEntry } from '$lib/api';
	import { splitEntryPath, shouldExpandEntry } from '$lib/filePath';
	import { liveEvent } from '$lib/liveUpdates';
	import { keyboardCursorPath } from '$lib/treeStore';
	import { getCachedDir, setCachedDir, prefetchTreePath } from '$lib/treeCache';

	export let source: string;
	export let entry: DirEntry;
	export let activePath: string | null = null;
	export let depth: number = 0;

	const dispatch = createEventDispatcher<{
		open: { source: string; path: string; kind: string; archivePath?: string };
	}>();

	let expanded = false;
	let children: DirEntry[] = [];
	let loaded = false;
	let loading = false;
	let loadError = false;
	let rowEl: HTMLElement | null = null;
	// Tracks the activePath value that last triggered auto-expand, so that a
	// manual collapse is not immediately overridden by the reactive below.
	let prevAutoExpandPath: string | null = null;

	$: if (entry.path === activePath && rowEl) {
		tick().then(() => rowEl?.scrollIntoView({ block: 'center', behavior: 'smooth' }));
	}

	// An archive file (kind='archive') can be expanded like a directory.
	$: isExpandable = entry.entry_type === 'dir' || entry.kind === 'archive';

	// Auto-expand when activePath changes to a new descendant value. By
	// gating on `activePath !== prevAutoExpandPath`, a manual collapse is not
	// immediately overridden: the reactive re-runs when `expanded` changes, but
	// the path comparison short-circuits and leaves `expanded` alone.
	$: if (isExpandable && activePath !== prevAutoExpandPath && activePath && shouldExpandEntry(entry, activePath)) {
		prevAutoExpandPath = activePath;
		if (!expanded) expandDir();
	}

	// Compute the prefix this directory row is responsible for.
	// entry.path for directories already has a trailing slash (e.g. "docs/plans/").
	$: myPrefix = entry.entry_type === 'dir' ? entry.path : null;

	// React to live index events: silently refresh children when this expanded
	// directory is an ancestor of a changed file. Checking ancestors (not just
	// the immediate parent) ensures a new subdirectory becomes visible in its
	// parent's listing even though the SSE path points deeper (e.g.
	// "dir/newsubdir/file.txt" must trigger a refresh of "dir/").
	$: if ($liveEvent && myPrefix && expanded && $liveEvent.source === source) {
		const ev = $liveEvent;
		if (ev.path.startsWith(myPrefix) || (ev.new_path && ev.new_path.startsWith(myPrefix))) {
			silentRefresh();
		}
	}

	async function silentRefresh() {
		try {
			const resp = await listDir(source, entry.path);
			children = resp.entries;
		} catch {
			// leave existing children on error
		}
	}

	function dirOf(p: string): string {
		const i = p.lastIndexOf('/');
		return i >= 0 ? p.slice(0, i + 1) : '';
	}

	async function expandDir() {
		if (!loaded) {
			loading = true;
			try {
				// The cache key for archive roots uses a trailing "::" (the prefix
				// used by list_dir / expandTreePath for archive root listings).
				const cacheKey = entry.kind === 'archive' ? entry.path + '::' : entry.path;
				let cached = getCachedDir(source, cacheKey);
				if (!cached && activePath) {
					// Warm all ancestor and archive-member levels in one request;
					// concurrent TreeRows share the same in-flight promise via treeCache.
					await prefetchTreePath(source, activePath);
					cached = getCachedDir(source, cacheKey);
				}
				if (cached) {
					children = cached;
				} else if (entry.kind === 'archive') {
					const resp = await listArchiveMembers(source, entry.path);
					children = resp.entries;
					setCachedDir(source, cacheKey, children);
				} else {
					const resp = await listDir(source, entry.path);
					children = resp.entries;
					setCachedDir(source, entry.path, children);
				}
				loaded = true;
			} catch {
				loadError = true;
			} finally {
				loading = false;
			}
		}
		expanded = true;
	}

	async function toggleDir(e: MouseEvent) {
		e.stopPropagation();
		if (!expanded) {
			await expandDir();
		} else {
			expanded = false;
		}
	}

	async function onDirRowClick() {
		// For archive nodes: expand to one level and dispatch open event
		if (entry.kind === 'archive') {
			if (!expanded) {
				await expandDir();
			}
			dispatch('open', {
				source,
				path: entry.path,
				kind: 'archive',
			});
		} else {
			// Regular directories: just toggle
			await toggleDir(new MouseEvent('click'));
		}
	}

	function openFile() {
		keyboardCursorPath.set(null); // activePath takes over the highlight
		const { path, archivePath } = splitEntryPath(entry.path);
		dispatch('open', { source, path, kind: entry.kind ?? 'text', archivePath });
		// Restore focus to this button after the file viewer renders, so that
		// arrow key navigation continues to work without re-clicking the tree.
		// Only steal it back if the user hasn't already moved focus to another tree item.
		setTimeout(() => {
			const tree = rowEl?.closest('[role="tree"]');
			if (tree && !tree.contains(document.activeElement)) rowEl?.focus();
		}, 0);
	}
</script>

<li class="row-item">
	{#if isExpandable}
		<div class="row row--dir"
			class:active={$keyboardCursorPath !== null ? $keyboardCursorPath === entry.path : entry.kind === 'archive' && entry.path === activePath}
			class:loading
			style="padding-left: {8 + depth * 16}px"
			bind:this={rowEl}
		>
			<button class="expand-arrow" on:click={toggleDir} title={expanded ? 'Collapse' : 'Expand'}>
				<span class="icon">{expanded ? '▾' : '▸'}</span>
			</button>
			<button class="dir-name" data-tree-nav="dir" data-tree-path={entry.path} on:click={onDirRowClick}>
				<span class="name">{entry.name}</span>
			</button>
		</div>
		{#if expanded}
			{#if loadError}
				<div class="load-error" style="padding-left: {8 + (depth + 1) * 16}px">Error loading</div>
			{:else if children.length === 0}
				<div class="empty-msg" style="padding-left: {8 + (depth + 1) * 16}px">Empty</div>
			{:else}
				<ul class="tree-list">
					{#each children as child (child.path)}
						<svelte:self
							source={source}
							entry={child}
							activePath={activePath}
							depth={depth + 1}
							on:open
						/>
					{/each}
				</ul>
			{/if}
		{/if}
	{:else}
		<button
			class="row row--file"
			class:active={$keyboardCursorPath !== null ? $keyboardCursorPath === entry.path : entry.path === activePath}
			style="padding-left: {8 + depth * 16}px"
			data-tree-nav="file"
			data-tree-path={entry.path}
			on:click={openFile}
			bind:this={rowEl}
		>
			<span class="icon kind-icon" title={entry.kind}>·</span>
			<span class="name">{entry.name}</span>
		</button>
	{/if}
</li>

<style>
	.row-item {
		list-style: none;
	}

	.tree-list {
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 0;
		width: 100%;
		background: none;
		border: none;
		padding-top: 2px;
		padding-bottom: 2px;
		padding-right: 8px;
		color: var(--text);
		font-size: 13px;
		white-space: nowrap;
		overflow: hidden;
	}

	.row--file {
		cursor: pointer;
		text-align: left;
		outline: none;
	}

	.row--file:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.06));
	}

	.row--file.active {
		background: var(--accent-subtle, rgba(88, 166, 255, 0.15));
		color: var(--accent, #58a6ff);
	}

	.row--dir {
		position: relative;
	}

	.row--dir.loading,
	.row--dir.loading .expand-arrow,
	.row--dir.loading .dir-name {
		cursor: wait;
	}

	.row--dir:hover,
	.row--dir:focus-within {
		background: var(--bg-hover, rgba(255, 255, 255, 0.06));
	}

	.row--dir.active {
		background: var(--accent-subtle, rgba(88, 166, 255, 0.15));
		color: var(--accent, #58a6ff);
	}

	.expand-arrow {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 14px;
		height: 100%;
		background: none;
		border: none;
		cursor: pointer;
		padding: 0;
		color: var(--text);
		flex-shrink: 0;
	}

	.expand-arrow:hover {
		opacity: 0.7;
	}

	.dir-name {
		display: flex;
		align-items: center;
		flex: 1;
		background: none;
		border: none;
		cursor: pointer;
		padding: 0;
		color: inherit;
		font-size: inherit;
		text-align: left;
		overflow: hidden;
		outline: none;
	}

	.icon {
		flex-shrink: 0;
		width: 14px;
		text-align: center;
		color: var(--text-muted);
		font-size: 11px;
	}

	.row--dir .icon {
		color: var(--text);
	}

	.expand-arrow .icon {
		font-size: 18px;
		width: 14px;
	}

	.name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.load-error {
		font-size: 12px;
		color: #f85149;
		padding-top: 2px;
		padding-bottom: 2px;
	}

	.empty-msg {
		font-size: 12px;
		color: var(--text-muted);
		padding-top: 2px;
		padding-bottom: 2px;
		font-style: italic;
	}
</style>

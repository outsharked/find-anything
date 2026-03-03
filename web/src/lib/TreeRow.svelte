<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';
	import { listDir, listArchiveMembers } from '$lib/api';
	import type { DirEntry } from '$lib/api';
	import { splitEntryPath, shouldExpandEntry } from '$lib/filePath';

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
	let loadError = false;
	let rowEl: HTMLElement | null = null;

	$: if (entry.path === activePath && rowEl) {
		tick().then(() => rowEl?.scrollIntoView({ block: 'center', behavior: 'smooth' }));
	}

	// An archive file (kind='archive') can be expanded like a directory.
	$: isExpandable = entry.entry_type === 'dir' || entry.kind === 'archive';

	// Auto-expand directories and archives if activePath is a descendant or exact match.
	$: if (isExpandable && activePath) {
		if (shouldExpandEntry(entry, activePath) && !expanded) {
			expandDir();
		}
	}

	async function expandDir() {
		if (!loaded) {
			try {
				const resp = entry.kind === 'archive'
					? await listArchiveMembers(source, entry.path)
					: await listDir(source, entry.path);
				children = resp.entries;
				loaded = true;
			} catch {
				loadError = true;
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
		const { path, archivePath } = splitEntryPath(entry.path);
		dispatch('open', { source, path, kind: entry.kind ?? 'text', archivePath });
	}
</script>

<li class="row-item">
	{#if isExpandable}
		<div class="row row--dir" class:active={entry.kind === 'archive' && entry.path === activePath} style="padding-left: {8 + depth * 16}px" bind:this={rowEl}>
			<button class="expand-arrow" on:click={toggleDir} title={expanded ? 'Collapse' : 'Expand'}>
				<span class="icon">{expanded ? '▾' : '▸'}</span>
			</button>
			<button class="dir-name" on:click={onDirRowClick}>
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
			class:active={entry.path === activePath}
			style="padding-left: {8 + depth * 16}px"
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

	.row--dir:hover {
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
		width: 18px;
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
		padding-left: 4px;
		color: inherit;
		font-size: inherit;
		text-align: left;
		overflow: hidden;
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

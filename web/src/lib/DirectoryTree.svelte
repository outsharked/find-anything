<script lang="ts">
	import { onMount } from 'svelte';
	import { listDir } from '$lib/api';
	import type { DirEntry } from '$lib/api';
	import TreeRow from '$lib/TreeRow.svelte';
	import { liveEvent } from '$lib/liveUpdates';
	import { getCachedDir, prefetchTreePath } from '$lib/treeCache';

	let {
		source,
		activePath = null
	}: {
		source: string;
		/** Currently open file path — highlighted in the tree. */
		activePath?: string | null;
	} = $props();

	let roots: DirEntry[] = $state([]);
	let loading = $state(true);
	let error: string | null = $state(null);

	onMount(async () => {
		try {
			// If an expand prefetch is already in-flight (fired before navigation),
			// await it so we use the cached root instead of making a separate request.
			const cached = getCachedDir(source, '');
			if (cached) {
				roots = cached;
			} else if (activePath) {
				await prefetchTreePath(source, activePath);
				roots = getCachedDir(source, '') ?? (await listDir(source, '')).entries;
			} else {
				roots = (await listDir(source, '')).entries;
			}
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	});

	// Refresh roots when a file at the root level is added, removed, or renamed.
	$effect(() => {
		if ($liveEvent && $liveEvent.source === source && !loading) {
			const ev = $liveEvent;
			const parentDir = dirOf(ev.path);
			const newParentDir = ev.new_path ? dirOf(ev.new_path) : null;
			if (parentDir === '' || newParentDir === '') {
				refreshRoots();
			}
		}
	});

	async function refreshRoots() {
		try {
			const resp = await listDir(source, '');
			roots = resp.entries;
		} catch {
			// leave existing roots on error
		}
	}

	function dirOf(p: string): string {
		const i = p.lastIndexOf('/');
		return i >= 0 ? p.slice(0, i + 1) : '';
	}
</script>

<div class="tree">
	{#if loading}
		<div class="tree-status">Loading…</div>
	{:else if error}
		<div class="tree-status tree-error">{error}</div>
	{:else if roots.length === 0}
		<div class="tree-status">No files indexed.</div>
	{:else}
		<ul class="tree-list">
			{#each roots as entry (entry.path)}
				<TreeRow {source} {entry} {activePath} depth={0} on:open />
			{/each}
		</ul>
	{/if}
</div>

<style>
	.tree {
		font-size: 13px;
		overflow-y: auto;
		height: 100%;
		padding: 4px 0;
		background: var(--bg-secondary);
		border-right: 1px solid var(--border);
	}

	.tree-status {
		padding: 12px;
		color: var(--text-muted);
	}

	.tree-error {
		color: #f85149;
	}

	.tree-list {
		list-style: none;
		margin: 0;
		padding: 0;
	}
</style>

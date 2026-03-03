<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { goto } from '$app/navigation';
	import SearchBox from '$lib/SearchBox.svelte';
	import SourceSelector from '$lib/SourceSelector.svelte';
	import PathBar from '$lib/PathBar.svelte';
	import FileViewer from '$lib/FileViewer.svelte';
	import DirListing from '$lib/DirListing.svelte';
	import type { LineSelection } from '$lib/lineSelection';
	import type { FilePath } from '$lib/filePath';

	export let fileSource: string;
	export let currentFile: FilePath | null;
	export let fileSelection: LineSelection;
	export let panelMode: 'file' | 'dir';
	export let currentDirPrefix: string;
	export let showTree: boolean;
	export let baseUrl: string | null;
	export let query: string;
	export let mode: string;
	export let searching: boolean;
	export let sources: string[];
	export let selectedSources: string[];

	const dispatch = createEventDispatcher<{
		back: void;
		search: { query: string; mode: string };
		sourceChange: string[];
		treeToggle: void;
		openFileFromTree: { source: string; path: string; kind: string; archivePath?: string; showAsDirectory?: boolean };
		openDirFile: { source: string; path: string; kind: string; archivePath?: string };
		openDir: { prefix: string };
		lineselect: { selection: LineSelection };
	}>();

	let isTyping = false;

	$: pathBarPath = panelMode === 'dir' ? currentDirPrefix : (currentFile?.outer ?? '');
</script>

<div class="topbar">
	<span class="logo">find-anything</span>
	<button
		class="tree-toggle"
		class:active={showTree}
		title="Toggle file tree (Ctrl+P to search files)"
		on:click={() => dispatch('treeToggle')}
	>◫</button>
	<div class="search-wrap">
		<SearchBox
			{query}
			{mode}
			{searching}
			bind:isTyping
			on:change={(e) => dispatch('search', e.detail)}
		/>
	</div>
	{#if sources.length > 0}
		<SourceSelector
			{sources}
			selected={selectedSources}
			on:change={(e) => dispatch('sourceChange', e.detail)}
		/>
	{/if}
	<button class="gear-btn" title="Settings" on:click={() => goto('/settings')}>⚙</button>
</div>

<div class="viewer-wrap">
	<PathBar
		source={fileSource}
		path={pathBarPath}
		archivePath={panelMode === 'file' ? currentFile?.inner ?? null : null}
		{baseUrl}
		on:back={() => dispatch('back')}
	/>
	{#if panelMode === 'dir'}
		<DirListing
			source={fileSource}
			prefix={currentDirPrefix}
			on:openFile={(e) => dispatch('openDirFile', e.detail)}
			on:openDir={(e) => dispatch('openDir', e.detail)}
		/>
	{:else if currentFile}
		{#key `${fileSource}:${currentFile.full}`}
			<FileViewer
				source={fileSource}
				path={currentFile.outer}
				archivePath={currentFile.inner}
				selection={fileSelection}
				on:lineselect={(e) => dispatch('lineselect', e.detail)}
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
		flex-wrap: nowrap;
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

	.viewer-wrap {
		flex: 1;
		overflow: hidden;
		display: flex;
		flex-direction: column;
	}
</style>

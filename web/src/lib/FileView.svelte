<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { goto } from '$app/navigation';
	import SearchBox from '$lib/SearchBox.svelte';
	import AdvancedSearch from '$lib/AdvancedSearch.svelte';
	import PathBar from '$lib/PathBar.svelte';
	import FileViewer from '$lib/FileViewer.svelte';
	import DirListing from '$lib/DirListing.svelte';
	import type { LineSelection } from '$lib/lineSelection';
	import type { FileViewState } from '$lib/appState';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';
	import SearchHelp from '$lib/SearchHelp.svelte';

	export let fileView: FileViewState;
	export let showTree: boolean;
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

	const dispatch = createEventDispatcher<{
		back: void;
		search: { query: string };
		filterChange: { sources: string[]; kinds: string[]; dateFrom?: number; dateTo?: number; caseSensitive: boolean; scope: SearchScope; matchType: SearchMatchType };
		treeToggle: void;
		openFileFromTree: { source: string; path: string; kind: string; archivePath?: string; showAsDirectory?: boolean };
		openDirFile: { source: string; path: string; kind: string; archivePath?: string };
		openDir: { prefix: string };
		lineselect: { selection: LineSelection };
		navigateDir: { prefix: string };
	}>();

	let isTyping = false;

	$: pathBarPath = fileView.panelMode === 'dir' ? fileView.dirPrefix : fileView.file.outer;
</script>

<div class="topbar">
	<span class="logo">find-anything</span>
	<button
		class="tree-toggle"
		class:active={showTree}
		title="Toggle file tree (Ctrl+P to search files)"
		on:click={() => dispatch('treeToggle')}
	>◫</button>
	<SearchHelp />
	<div class="search-wrap">
		<SearchBox
			{query}
			{searching}
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

<div class="viewer-wrap">
	<PathBar
		source={fileView.source}
		path={pathBarPath}
		archivePath={fileView.panelMode === 'file' ? fileView.file.inner ?? null : null}
		on:back={() => dispatch('back')}
		on:navigate={(e) => {
			if (e.detail.type === 'dir') {
				dispatch('openDir', { prefix: e.detail.prefix });
			} else {
				dispatch('openFileFromTree', { source: fileView.source, path: e.detail.path, kind: e.detail.kind });
			}
		}}
	/>
	{#if fileView.panelMode === 'dir'}
		<DirListing
			source={fileView.source}
			prefix={fileView.dirPrefix}
			on:openFile={(e) => dispatch('openDirFile', e.detail)}
			on:openDir={(e) => dispatch('openDir', e.detail)}
		/>
	{:else}
		{#key `${fileView.source}:${fileView.file.full}`}
			<FileViewer
				source={fileView.source}
				path={fileView.file.outer}
				archivePath={fileView.file.inner}
				selection={fileView.selection}
				preferOriginal={fileView.selection.length === 0}
				on:lineselect={(e) => dispatch('lineselect', e.detail)}
				on:open={(e) => dispatch('openDirFile', e.detail)}
				on:navigateDir={(e) => dispatch('openDir', e.detail)}
				on:navigate={(e) => dispatch('openFileFromTree', { source: fileView.source, path: e.detail.path, kind: 'unknown' })}
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

<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { goto } from '$app/navigation';
	import PathBar from '$lib/PathBar.svelte';
	import FileViewer from '$lib/FileViewer.svelte';
	import DirListing from '$lib/DirListing.svelte';
	import type { LineSelection } from '$lib/lineSelection';
	import type { FileViewState } from '$lib/appState';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';
	import TopBar from '$lib/TopBar.svelte';

	export let fileView: FileViewState;
	export let showBack = true;
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

	$: pathBarPath = fileView.panelMode === 'dir' ? fileView.dirPrefix : fileView.file.outer;
</script>

<TopBar
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
	on:search={(e) => dispatch('search', e.detail)}
	on:treeToggle={() => dispatch('treeToggle')}
	on:filterChange={(e) => dispatch('filterChange', e.detail)}
/>

<div class="viewer-wrap">
	<PathBar
		source={fileView.source}
		path={pathBarPath}
		archivePath={fileView.panelMode === 'file' ? fileView.file.inner ?? null : null}
		{showBack}
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
	.viewer-wrap {
		flex: 1;
		overflow: hidden;
		display: flex;
		flex-direction: column;
	}
</style>

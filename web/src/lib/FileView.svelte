<script lang="ts">
	import { goto } from '$app/navigation';
	import PathBar from '$lib/PathBar.svelte';
	import FileViewer from '$lib/FileViewer.svelte';
	import DirListing from '$lib/DirListing.svelte';
	import type { LineSelection } from '$lib/lineSelection';
	import type { FileViewState } from '$lib/appState';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';
	import TopBar from '$lib/TopBar.svelte';

	type FilterChangeDetail = { sources: string[]; kinds: string[]; dateFrom?: number; dateTo?: number; caseSensitive: boolean; scope: SearchScope; matchType: SearchMatchType };
	type OpenFileFromTreeDetail = { source: string; path: string; kind: string; archivePath?: string; showAsDirectory?: boolean };
	type OpenDirFileDetail = { source: string; path: string; kind: string; archivePath?: string };

	let {
		fileView,
		showBack = true,
		showTree,
		query,
		scope = 'line',
		matchType = 'fuzzy',
		searching,
		sources,
		selectedSources,
		selectedKinds = [],
		dateFrom = '',
		dateTo = '',
		caseSensitive = false,
		onBack,
		onSearch,
		onFilterChange,
		onTreeToggle,
		onOpenFileFromTree,
		onOpenDirFile,
		onOpenDir,
		onLineSelect
	}: {
		fileView: FileViewState;
		showBack?: boolean;
		showTree: boolean;
		query: string;
		scope?: SearchScope;
		matchType?: SearchMatchType;
		searching: boolean;
		sources: string[];
		selectedSources: string[];
		selectedKinds?: string[];
		dateFrom?: string;
		dateTo?: string;
		caseSensitive?: boolean;
		onBack?: () => void;
		onSearch?: (detail: { query: string }) => void;
		onFilterChange?: (detail: FilterChangeDetail) => void;
		onTreeToggle?: () => void;
		onOpenFileFromTree?: (detail: OpenFileFromTreeDetail) => void;
		onOpenDirFile?: (detail: OpenDirFileDetail) => void;
		onOpenDir?: (detail: { prefix: string }) => void;
		onLineSelect?: (detail: { selection: LineSelection }) => void;
	} = $props();

	let pathBarPath = $derived(fileView.panelMode === 'dir' ? fileView.dirPrefix : fileView.file.outer);
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
	{onSearch}
	{onTreeToggle}
	{onFilterChange}
/>

<div class="viewer-wrap">
	<PathBar
		source={fileView.source}
		path={pathBarPath}
		archivePath={fileView.panelMode === 'file' ? fileView.file.inner ?? null : null}
		{showBack}
		onBack={() => onBack?.()}
		onNavigate={(action) => {
			if (action.type === 'dir') {
				onOpenDir?.({ prefix: action.prefix });
			} else {
				onOpenFileFromTree?.({ source: fileView.source, path: action.path, kind: action.kind });
			}
		}}
	/>
	{#if fileView.panelMode === 'dir'}
		<DirListing
			source={fileView.source}
			prefix={fileView.dirPrefix}
			onOpenFile={(detail) => onOpenDirFile?.(detail)}
			onOpenDir={(detail) => onOpenDir?.(detail)}
		/>
	{:else}
		{#key `${fileView.source}:${fileView.file.full}`}
			<FileViewer
				source={fileView.source}
				path={fileView.file.outer}
				archivePath={fileView.file.inner}
				selection={fileView.selection}
				preferOriginal={fileView.selection.length === 0}
				on:lineselect={(e) => onLineSelect?.(e.detail)}
				on:open={(e) => onOpenDirFile?.(e.detail)}
				on:navigateDir={(e) => onOpenDir?.(e.detail)}
				on:navigate={(e) => onOpenFileFromTree?.({ source: fileView.source, path: e.detail.path, kind: 'unknown' })}
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

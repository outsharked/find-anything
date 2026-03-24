<script lang="ts">
	import { createEventDispatcher, onMount, onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import SearchBox from '$lib/SearchBox.svelte';
	import AdvancedSearch from '$lib/AdvancedSearch.svelte';
	import SearchHelp from '$lib/SearchHelp.svelte';
	import AppLogo from '$lib/AppLogo.svelte';
	import MobilePanel from '$lib/MobilePanel.svelte';
	import SearchHelpContent from '$lib/SearchHelpContent.svelte';
	import DirTypeahead from '$lib/DirTypeahead.svelte';
	import { listDir } from '$lib/api';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';

	export let query: string;
	export let searching: boolean;
	export let showTree: boolean;
	export let sources: string[] = [];
	export let selectedSources: string[] = [];
	export let selectedKinds: string[] = [];
	export let dateFrom = '';
	export let dateTo = '';
	export let caseSensitive = false;
	export let scope: SearchScope = 'line';
	export let matchType: SearchMatchType = 'fuzzy';
	export let nlpDetectedPhrase: string | undefined = undefined;

	const dispatch = createEventDispatcher<{
		search: { query: string };
		treeToggle: void;
		filterChange: { sources: string[]; kinds: string[]; dateFrom?: number; dateTo?: number; caseSensitive: boolean; scope: SearchScope; matchType: SearchMatchType };
	}>();

	export let isSearchActive = false;

	let helpOpen = false;
	let isTyping = false;
	$: isSearchActive = isTyping || searching;
	$: nlpHighlightSpan = (() => {
		if (!nlpDetectedPhrase || isTyping) return undefined;
		const idx = query.toLowerCase().indexOf(nlpDetectedPhrase.toLowerCase());
		if (idx === -1) return undefined;
		return [idx, idx + nlpDetectedPhrase.length] as [number, number];
	})();

	let searchBox: SearchBox;
	export function focus() { searchBox?.focus(); }

	// ── Dir typeahead ──────────────────────────────────────────────────────────

	let liveQuery = query;
	let searchFocused = false;
	let taOpen = false;
	let taItems: string[] = [];
	let taActiveIdx = -1;
	let taLoading = false;
	let taSourcePhase = false;
	/**
	 * The resolved token currently displayed in the typeahead (may differ from
	 * liveQuery when auto-advance jumped ahead). Used by selectItem so it always
	 * builds the next token from the correct resolved position.
	 * Reset to null whenever the user types (liveQuery changes from rawInput).
	 */
	let activeToken: string | null = null;

	// Simple in-memory cache: "source:prefix" → dir names
	const dirCache = new Map<string, string[]>();

	/** Extract the source: token being typed (last whitespace-delimited token). */
	function getDirToken(q: string): string | null {
		if (q !== q.trimEnd()) return null;
		const last = q.split(/\s+/).pop() ?? '';
		return last.startsWith('source:') ? last : null;
	}

	/** Parse a source: token into { phase, filter, source?, parentPath? }. */
	function parseDirToken(token: string) {
		const rest = token.slice(7).replace(/^\/+/, '');
		const lastSlash = rest.lastIndexOf('/');
		if (lastSlash === -1) {
			return { phase: 'source' as const, filter: rest };
		}
		const beforeSlash = rest.slice(0, lastSlash);
		const firstSlash = beforeSlash.indexOf('/');
		const src = firstSlash === -1 ? beforeSlash : beforeSlash.slice(0, firstSlash);
		const parentPath = firstSlash === -1 ? '' : beforeSlash.slice(firstSlash + 1);
		return { phase: 'dir' as const, source: src, parentPath, filter: rest.slice(lastSlash + 1) };
	}

	/** Replace the source: token at the end of a query string with newToken. */
	function replaceLastDirToken(q: string, newToken: string): string {
		const parts = q.trimEnd().split(/\s+/);
		parts[parts.length - 1] = newToken;
		return parts.join(' ');
	}

	/**
	 * Recursively fetch dirs, auto-advancing through single-option levels.
	 * Returns the deepest path reached and the dirs at that level.
	 * Pure: no UI state mutations — caller applies results once at the end.
	 */
	async function resolveAutoPath(
		source: string,
		path: string,
		id: number
	): Promise<{ path: string; dirs: string[] } | null> {
		const cacheKey = `${source}:${path}`;
		let dirs: string[];
		if (dirCache.has(cacheKey)) {
			dirs = dirCache.get(cacheKey)!;
		} else {
			const prefix = path ? path + '/' : '';
			try {
				const resp = await listDir(source, prefix);
				dirs = resp.entries.filter(e => e.entry_type === 'dir').map(e => e.name);
				dirCache.set(cacheKey, dirs);
			} catch {
				dirs = [];
			}
		}
		if (id !== updateId) return null; // stale — abort
		if (dirs.length === 1) {
			// Single option: silently advance one level and recurse.
			const next = path ? `${path}/${dirs[0]}` : dirs[0];
			return resolveAutoPath(source, next, id);
		}
		return { path, dirs }; // 0 or 2+ options: stop here
	}

	let updateId = 0;

	async function updateTypeahead(token: string) {
		const id = ++updateId;
		taOpen = true;
		taLoading = true;
		taItems = [];

		const parsed = parseDirToken(token);

		if (parsed.phase === 'source') {
			taSourcePhase = true;
			const f = parsed.filter.toLowerCase();
			const matches = sources.filter(s => s.toLowerCase().startsWith(f));
			if (id !== updateId) return;

			if (matches.length !== 1) {
				// 0 or 2+ sources: show list directly.
				taLoading = false;
				taItems = matches;
				taActiveIdx = -1;
				return;
			}

			// Exactly one source: resolve the full auto-advance path.
			const src = matches[0];
			const resolved = await resolveAutoPath(src, '', id);
			if (!resolved || id !== updateId) return;

			const finalToken = resolved.path
				? `source:${src}/${resolved.path}/`
				: `source:${src}/`;
			activeToken = finalToken;
			liveQuery = replaceLastDirToken(liveQuery, finalToken);
			dispatch('search', { query: liveQuery });

			taSourcePhase = false;
			taLoading = false;
			taItems = resolved.dirs;
			taActiveIdx = -1;
			taOpen = resolved.dirs.length > 0;
			if (resolved.dirs.length === 0) liveQuery += ' '; // leaf: add space to close

		} else {
			taSourcePhase = false;
			const { source, parentPath, filter } = parsed;

			// Resolve auto-advance from current dir level.
			const resolved = await resolveAutoPath(source, parentPath, id);
			if (!resolved || id !== updateId) return;

			if (resolved.path !== parentPath) {
				// Path advanced: update the displayed token.
				const finalToken = `source:${source}/${resolved.path}/`;
				activeToken = finalToken;
				liveQuery = replaceLastDirToken(liveQuery, finalToken);
				dispatch('search', { query: liveQuery });
			} else {
				activeToken = token;
			}

			const f = filter.toLowerCase();
			const items = resolved.dirs.filter(d => d.toLowerCase().startsWith(f));
			taLoading = false;
			taItems = items;
			taActiveIdx = -1;
			taOpen = items.length > 0;
			if (items.length === 0) liveQuery += ' '; // leaf: add space to close
		}
	}

	function closeTypeahead() {
		taOpen = false;
		taItems = [];
		taActiveIdx = -1;
		activeToken = null;
	}

	function selectItem(name: string) {
		// Use the resolved token (may differ from liveQuery if auto-advance jumped ahead).
		const token = activeToken ?? getDirToken(liveQuery);
		if (!token) return;
		const parsed = parseDirToken(token);
		let newToken: string;
		if (parsed.phase === 'source') {
			newToken = `source:${name}/`;
		} else {
			const base = parsed.parentPath
				? `${parsed.source}/${parsed.parentPath}/${name}`
				: `${parsed.source}/${name}`;
			newToken = `source:${base}/`;
		}
		activeToken = null; // reset so reactive block re-runs updateTypeahead for next level
		liveQuery = replaceLastDirToken(liveQuery, newToken);
		dispatch('search', { query: liveQuery });
		taActiveIdx = -1;
	}

	// React to live query changes. Guard: skip when liveQuery was updated by
	// auto-advance (activeToken set) to avoid re-triggering updateTypeahead.
	$: {
		const token = getDirToken(liveQuery);
		if (token && searchFocused && token !== activeToken) {
			updateTypeahead(token);
		} else if (!token) {
			closeTypeahead();
		}
	}

	// Sync liveQuery when the query PROP changes externally (e.g. chip removed).
	let _prevQuery = query;
	$: if (query !== _prevQuery) {
		_prevQuery = query;
		if (!taOpen) liveQuery = query;
	}

	function handleTypeaheadKeydown(e: KeyboardEvent) {
		if (!taOpen || !searchFocused) return;
		if (e.key === 'ArrowDown') {
			e.preventDefault();
			taActiveIdx = Math.min(taActiveIdx + 1, taItems.length - 1);
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			taActiveIdx = Math.max(taActiveIdx - 1, -1);
		} else if ((e.key === 'Enter' || e.key === 'Tab') && taActiveIdx >= 0) {
			e.preventDefault();
			e.stopImmediatePropagation();
			selectItem(taItems[taActiveIdx]);
		} else if (e.key === 'Escape') {
			e.preventDefault();
			closeTypeahead();
		}
	}

	onMount(() => {
		document.addEventListener('keydown', handleTypeaheadKeydown, true);
	});
	onDestroy(() => {
		document.removeEventListener('keydown', handleTypeaheadKeydown, true);
	});
</script>

<div class="topbar">
	<button class="logo-btn" on:click={() => (helpOpen = true)} aria-label="Search help">
		<AppLogo />
	</button>
	<button
		class="tree-toggle"
		class:active={showTree}
		data-tooltip="Toggle file tree"
		on:click={() => dispatch('treeToggle')}
	>◫</button>
	<div class="help-wrap-outer"><SearchHelp bind:open={helpOpen} /></div>
	<div class="search-wrap">
		<SearchBox
			bind:this={searchBox}
			{query}
			searching={isSearchActive}
			{nlpHighlightSpan}
			bind:isTyping
			on:change={(e) => dispatch('search', { query: e.detail.query })}
			on:rawInput={(e) => { liveQuery = e.detail.query; }}
			on:focus={() => { searchFocused = true; }}
			on:blur={() => { searchFocused = false; }}
		/>
		{#if taOpen}
			<DirTypeahead
				items={taItems}
				activeIndex={taActiveIdx}
				loading={taLoading}
				sourcePhase={taSourcePhase}
				on:select={(e) => selectItem(e.detail.name)}
				on:hover={(e) => { taActiveIdx = e.detail.index; }}
			/>
		{/if}
	</div>
	{#if sources.length > 0}
		<div class="advanced-wrap">
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
		</div>
	{/if}
	<button class="gear-btn" on:click={() => goto('/settings')}>⚙</button>
</div>

<MobilePanel bind:open={helpOpen} title="Search Help">
	<SearchHelpContent />
</MobilePanel>

<style>
	.topbar {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 16px;
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
		position: sticky;
		top: 0;
		z-index: 100;
	}

	.logo-btn {
		background: none;
		border: none;
		padding: 0;
		cursor: default;
		flex-shrink: 0;
	}

	.advanced-wrap { display: contents; }

	.search-wrap {
		min-width: 260px;
		flex: 1;
		position: relative;
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
		position: relative;
	}

	.tree-toggle:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
		color: var(--text);
	}

	.tree-toggle.active { color: var(--accent, #58a6ff); }

	.tree-toggle[data-tooltip]::after {
		content: attr(data-tooltip);
		position: absolute;
		top: calc(100% + 4px);
		left: 50%;
		transform: translateX(-50%);
		white-space: nowrap;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		color: var(--text-muted);
		padding: 2px 6px;
		border-radius: 3px;
		font-size: 11px;
		opacity: 0;
		pointer-events: none;
		transition: opacity 0.1s;
		z-index: 100;
	}

	.tree-toggle[data-tooltip]:hover::after { opacity: 1; }

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

	@media (max-width: 768px) {
		.topbar { gap: 6px; padding: 6px 10px; }
		.tree-toggle { display: none; }
		.help-wrap-outer { display: none; }
		.logo-btn { cursor: pointer; order: 1; }
		.search-wrap { order: 2; flex: 1 1 0; min-width: 0; }
		.advanced-wrap { order: 3; display: block; }
		.gear-btn { order: 4; min-width: 36px; min-height: 36px; display: flex; align-items: center; justify-content: center; }
	}
</style>

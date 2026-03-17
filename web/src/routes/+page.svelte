<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { pushState as svelteKitPushState, replaceState as svelteKitReplaceState, afterNavigate } from '$app/navigation';
	import { page } from '$app/stores';
	import { get } from 'svelte/store';
	import SearchView from '$lib/SearchView.svelte';
	import FileView from '$lib/FileView.svelte';
	import CommandPalette from '$lib/CommandPalette.svelte';
	import MultiSourceTree from '$lib/MultiSourceTree.svelte';
	import { search, listSources, getSettings, activateSession, AuthError } from '$lib/api';
	import type { SearchResult, SourceInfo } from '$lib/api';
	import { getToken, setToken } from '$lib/token';
	import { startLiveUpdates, liveEvent } from '$lib/liveUpdates';
	import { contextWindow, maxMarkdownRenderKb, fileViewPageSize } from '$lib/settingsStore';
	import { formatHash } from '$lib/lineSelection';
	import type { LineSelection } from '$lib/lineSelection';
	import { FilePath } from '$lib/filePath';
	import { buildUrl, restoreFromParams, serializeState, deserializeState, expandFileView, collapseFileView } from '$lib/appState';
	import { mergePage } from '$lib/pagination';
	import type { AppState, SerializedAppState, FileViewState } from '$lib/appState';
	import { profile } from '$lib/profile';
	import { parseNlpQuery } from '$lib/nlpQuery';
	import type { NlpResult } from '$lib/nlpQuery';
	import { parseSearchPrefixes, toServerMode, fromServerMode } from '$lib/searchPrefixes';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';

	// SvelteKit passes params to every layout/page component. Declare it to avoid
	// the runtime "unknown prop" warning. Assigned to _params to signal that it
	// is intentionally unused (access via $page.params if ever needed).
	export let params: Record<string, string>;
	const _params = params;

	// ── View state machine ────────────────────────────────────────────────────
	//
	// The main panel is a two-state discriminated union:
	//   search  (fileView === null)  →  <SearchView> + pagination
	//   file    (fileView !== null)  →  <FileView>   (file or directory)
	//
	// Two orthogonal overlays compose on top of either panel state:
	//   showTree     — left sidebar (independent; persists across transitions)
	//   showPalette  — Ctrl+P command-palette modal (independent)
	//
	// All transitions go through openFileView() / handleBack() so the
	// push-history step is never forgotten.

	/** Non-null when the file/directory viewer is open. */
	let fileView: FileViewState | null = null;
	let searchView: SearchView;
	let query = '';
	let scope: SearchScope = 'line';
	let matchType: SearchMatchType = 'fuzzy';

	let sources: SourceInfo[] = [];
	let selectedSources: string[] = [];
	let selectedKinds: string[] = [];
	let caseSensitive = false;
	// ISO date strings bound to the AdvancedSearch inputs (propagated back for controlled state).
	let dateFromStr = '';
	let dateToStr = '';
	// Unix timestamp equivalents sent to the API (undefined = no filter).
	let dateFromTs: number | undefined;
	let dateToTs: number | undefined;

	// NLP-extracted date state.
	let nlpResult: NlpResult | null = null;
	let nlpSuppressed = false;
	// Effective dates used in API calls (manual wins over NLP).
	let effectiveDateFrom: number | undefined;
	let effectiveDateTo: number | undefined;

	let results: SearchResult[] = [];
	let totalResults = 0;
	let searching = false;
	let searchError: string | null = null;
	let searchId = 0;

	let showTree = true;
	let showPalette = false;

	// Live index update state
	let resultsStale = false;
	let deletedPaths = new Set<string>();
	// Tracks the last $liveEvent object we acted on. Prevents re-processing the
	// same event when doSearch resets deletedPaths (which would otherwise
	// re-trigger this reactive block and immediately re-set resultsStale = true).
	let lastHandledEvent: typeof $liveEvent = null;

	$: if ($liveEvent && $liveEvent !== lastHandledEvent) {
		lastHandledEvent = $liveEvent;
		const ev = $liveEvent;
		const sourceMatches = selectedSources.length === 0 || selectedSources.includes(ev.source);
		if (sourceMatches && query.trim().length >= 3) {
			if (ev.action === 'deleted') {
				deletedPaths = new Set([...deletedPaths, `${ev.source}:${ev.path}`]);
			} else {
				resultsStale = true;
			}
		}
	}

	let sidebarWidth: number = $profile.sidebarWidth ?? 240;

	// ── Token setup ──────────────────────────────────────────────────────────────

	let showTokenSetup = false;
	let tokenInput = '';

	function checkToken() {
		if (!getToken()) showTokenSetup = true;
	}

	function saveToken() {
		if (!tokenInput.trim()) return;
		setToken(tokenInput.trim());
		tokenInput = '';
		showTokenSetup = false;
		// Set the session cookie so browser-native requests (e.g. <img src>) work.
		activateSession();
		// Re-run initial data load now that we have a token.
		initialLoad();
	}

	async function initialLoad() {
		try { sources = await listSources(); } catch (e) {
			if (e instanceof AuthError) { showTokenSetup = true; return; }
		}
		try {
			const s = await getSettings();
			// Use profile override if set; fall back to server default.
			const profileWindow = get(profile).contextWindow;
			contextWindow.set(profileWindow ?? s.context_window);
			maxMarkdownRenderKb.set(s.max_markdown_render_kb ?? 512);
			fileViewPageSize.set(s.file_view_page_size ?? 2000);
		} catch { /* silent */ }
	}

	// ── History ─────────────────────────────────────────────────────────────────

	function captureState(): AppState {
		return { query, mode: toServerMode(scope, matchType), selectedSources, ...expandFileView(fileView) };
	}

	function pushState() {
		const s = captureState();
		svelteKitPushState(buildUrl(s) + formatHash(s.fileSelection), serializeState(s));
	}

	// Like pushState but replaces the current history entry instead of adding one.
	// Used during search so that typing doesn't flood the back-button history.
	// Uses native history.replaceState (bypassing SvelteKit's navigation state
	// machine) to avoid rapid calls leaving SvelteKit stuck in a "navigating"
	// state that blocks pointer and keyboard events.
	function replaceSearchState() {
		const s = captureState();
		const url = buildUrl(s) + formatHash(s.fileSelection);
		history.replaceState(history.state, '', url);
	}

	function syncHash() {
		const hash = formatHash(fileView?.selection ?? []);
		const base = location.pathname + location.search;
		svelteKitReplaceState(hash ? base + hash : base, get(page).state);
	}

	function applyState(s: AppState) {
		fileView = collapseFileView(s);
		query = s.query;
		const restored = fromServerMode(s.mode);
		scope = restored.scope;
		matchType = restored.matchType;
		selectedSources = s.selectedSources;
		if (s.query) doSearch(s.query, s.selectedSources, false);
	}

	// ── Lifecycle ───────────────────────────────────────────────────────────────

	// Handle browser back/forward through states pushed by pushState() above.
	// afterNavigate fires after SvelteKit has updated $page.state, so reading
	// get(page).state here gives the restored AppState. The 'popstate' type
	// covers both deep history entries (state present) and entries predating our
	// app (state absent — fall back to URL params).
	afterNavigate(({ type }) => {
		if (type !== 'popstate') return;
		const s = get(page).state as SerializedAppState;
		if (s?.view) applyState(deserializeState(s));
		else applyState(restoreFromParams(new URLSearchParams(location.search)));
	});

	onMount(() => {
		const stopLive = startLiveUpdates();

		(async () => {
			checkToken();
			if (!showTokenSetup) {
				// Ensure the session cookie is set so browser-native requests work.
				activateSession();
				await initialLoad();
			}

			const params = new URLSearchParams(location.search);
			if (params.has('q') || params.has('path')) {
				const restored = restoreFromParams(params);
				showTree = restored.showTree;
				applyState(restored);
				svelteKitReplaceState(location.href, serializeState(captureState()));
			}
		})();

		function handleKeydown(e: KeyboardEvent) {
			if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'p') {
				e.preventDefault();
				showPalette = !showPalette;
			}
		}

		window.addEventListener('keydown', handleKeydown, { capture: true });
		// Scroll events as a secondary trigger: when the window is scrollable
		// and the user scrolls near the sentinel, load more results.
		mainContent.addEventListener('scroll', checkScroll, { passive: true });
		return () => {
			stopLive();
			window.removeEventListener('keydown', handleKeydown, { capture: true });
			mainContent.removeEventListener('scroll', checkScroll);
		};
	});

	// ── Load more ───────────────────────────────────────────────────────────────

	let mainContent: HTMLElement;
	let savedScrollTop = 0;
	let loadingMore = false;
	let noMoreResults = false;
	// Tracks server cursor independently of results.length. Client dedup can
	// reduce how many items are added per page; using results.length as the
	// offset would then re-request the same range and stall pagination.
	let loadOffset = 0;
	let sentinel: HTMLElement | null = null;

	// getBoundingClientRect() forces a synchronous layout reflow and returns
	// position relative to the viewport — reliable regardless of scroll container
	// or CSS layout (unlike scrollHeight - scrollY - innerHeight which breaks when
	// html/body have height:100%).
	function isNearBottom(): boolean {
		if (!sentinel) return false;
		return sentinel.getBoundingClientRect().top < window.innerHeight + 600;
	}

	function checkScroll() {
		if (loadingMore || noMoreResults || fileView !== null || query.trim().length < 3) return;
		if (isNearBottom()) triggerLoad();
	}

	async function triggerLoad() {
		if (loadingMore || noMoreResults || query.trim().length < 3) return;
		loadingMore = true;
		try {
			const prefixResult = parseSearchPrefixes(query);
			const effectiveScope = prefixResult.scopeOverride ?? scope;
			const effectiveMatch = prefixResult.matchOverride ?? matchType;
			const effectiveKindsLoad = prefixResult.kindsOverride ?? selectedKinds;
			const serverMode = toServerMode(effectiveScope, effectiveMatch);
			const resp = await search({ q: nlpResult?.query ?? prefixResult.query, mode: serverMode, sources: selectedSources, kinds: effectiveKindsLoad, limit: 50, offset: loadOffset, dateFrom: effectiveDateFrom, dateTo: effectiveDateTo, caseSensitive });
			if (resp.results.length === 0) {
				noMoreResults = true;
			} else {
				// IMPORTANT: client-side dedup must not be removed. The server
				// deduplicates within each request, but cross-page duplicates occur
				// because scoring_limit grows with each page (offset + limit + 200),
				// causing the server to re-rank candidates. An item at position 45 on
				// page 0 can shift to position 69 on page 1 and appear in both.
				// Duplicate keys in the keyed {#each} throw a runtime error and prevent
				// DOM updates, which keeps the sentinel pinned and causes an infinite
				// request loop. See CLAUDE.md §"Search result keys and load-more dedup".
				const merged = mergePage(results, resp.results, loadOffset);
				results = merged.results;
				totalResults = resp.total;
				loadOffset = merged.newOffset;
			}
			await tick();
		} catch { /* silent */ }
		loadingMore = false;
		// getBoundingClientRect() forces layout, so this is accurate after tick().
		// If sentinel is still near the bottom, keep filling the viewport.
		if (isNearBottom()) triggerLoad();
	}

	// ── Search ──────────────────────────────────────────────────────────────────

	async function doSearch(q: string, srcs: string[], push = true) {
		resultsStale = false;
		deletedPaths = new Set();
		if (q.trim().length < 3) {
			results = []; totalResults = 0; noMoreResults = false; loadOffset = 0; searchError = null;
			return;
		}

		// Parse query prefixes; prefixes override Advanced panel settings for this call.
		const prefixResult = parseSearchPrefixes(q);
		const effectiveScope = prefixResult.scopeOverride ?? scope;
		const effectiveMatch = prefixResult.matchOverride ?? matchType;
		const effectiveKinds = prefixResult.kindsOverride ?? selectedKinds;
		const serverMode = toServerMode(effectiveScope, effectiveMatch);
		const baseQuery = prefixResult.query;

		// NLP parse: extract dates + clean stop words. Skip if user dismissed.
		nlpResult = nlpSuppressed ? null : parseNlpQuery(baseQuery, serverMode);
		// Manual date range always wins; NLP fills in when no manual range is set.
		effectiveDateFrom = dateFromTs ?? nlpResult?.dateFrom;
		effectiveDateTo = dateToTs ?? nlpResult?.dateTo;
		const apiQuery = nlpResult?.query ?? baseQuery;

		searching = true;
		searchError = null;
		searchId += 1;
		const mySearchId = searchId;
		noMoreResults = false;
		loadOffset = 0;
		if (push) {
			replaceSearchState();
			window.scrollTo(0, 0);
		}
		try {
			const resp = await search({ q: apiQuery, mode: serverMode, sources: srcs, kinds: effectiveKinds, limit: 50, offset: 0, dateFrom: effectiveDateFrom, dateTo: effectiveDateTo, caseSensitive });
			if (mySearchId !== searchId) return;
			const merged = mergePage([], resp.results, 0);
			results = merged.results;
			totalResults = resp.total;
			loadOffset = merged.newOffset;
			if (resp.results.length === 0) noMoreResults = true;
			if (push) fileView = null;
		} catch (e) {
			if (mySearchId !== searchId) return;
			searchError = String(e);
			results = []; totalResults = 0; noMoreResults = true; loadOffset = 0;
			if (push) fileView = null;
		} finally {
			if (mySearchId === searchId) searching = false;
		}
		// Auto-fill viewport if the first page doesn't reach the scroll threshold.
		await tick();
		// Restore focus to the search box when transitioning from file view back to results.
		if (push && fileView === null) searchView?.focus();
		if (isNearBottom()) triggerLoad();
	}

	// ── Search event handlers ────────────────────────────────────────────────────

	function handleSearch(e: CustomEvent<{ query: string }>) {
		// New query text = fresh NLP parse (clear any prior suppression).
		if (e.detail.query !== query) nlpSuppressed = false;
		query = e.detail.query;
		doSearch(query, selectedSources);
	}

	function handleClearNlpDate() {
		nlpSuppressed = true;
		nlpResult = null;
		effectiveDateFrom = dateFromTs;
		effectiveDateTo = dateToTs;
		doSearch(query, selectedSources);
	}

	function handleFilterChange(e: CustomEvent<{ sources: string[]; kinds: string[]; dateFrom?: number; dateTo?: number; caseSensitive: boolean; scope: SearchScope; matchType: SearchMatchType }>) {
		selectedSources = e.detail.sources;
		selectedKinds = e.detail.kinds;
		caseSensitive = e.detail.caseSensitive;
		scope = e.detail.scope;
		matchType = e.detail.matchType;
		dateFromTs = e.detail.dateFrom;
		dateToTs = e.detail.dateTo;
		// Keep ISO strings in sync so AdvancedSearch inputs remain controlled.
		dateFromStr = dateFromTs != null ? new Date(dateFromTs * 1000).toISOString().slice(0, 10) : '';
		dateToStr = dateToTs != null ? new Date(dateToTs * 1000).toISOString().slice(0, 10) : '';
		if (query.trim()) doSearch(query, selectedSources);
	}

	// ── File viewer event handlers ───────────────────────────────────────────────

	/** Transition to file/directory panel state and push a history entry. */
	function openFileView(fv: FileViewState) {
		fileView = fv;
		pushState();
	}

	function openFile(e: CustomEvent<SearchResult>) {
		const r = e.detail;
		const file = FilePath.fromParts(r.path, r.archive_path ?? null);
		const extraLines = (r.extra_matches ?? [])
			.map((m) => m.line_number)
			.filter((n) => n > 0 && n !== r.line_number);
		const selection: LineSelection = r.line_number
			? [r.line_number, ...extraLines]
			: extraLines.length ? extraLines : [];
		savedScrollTop = mainContent?.scrollTop ?? 0;
		openFileView({ source: r.source, file, selection, panelMode: 'file', dirPrefix: '' });
	}

	function handleOpenFileFromTree(e: CustomEvent<{ source: string; path: string; kind: string; archivePath?: string; showAsDirectory?: boolean }>) {
		const file = FilePath.fromParts(e.detail.path, e.detail.archivePath ?? null);
		if (e.detail.showAsDirectory) {
			openFileView({ source: e.detail.source, file, selection: [], panelMode: 'dir', dirPrefix: file.full + '::' });
		} else {
			openFileView({ source: e.detail.source, file, selection: [], panelMode: 'file', dirPrefix: '' });
		}
	}

	function handleOpenDirFile(e: CustomEvent<{ source: string; path: string; kind: string; archivePath?: string }>) {
		const file = FilePath.fromParts(e.detail.path, e.detail.archivePath ?? null);
		openFileView({ ...(fileView!), file, selection: [], panelMode: 'file' });
	}

	function handleOpenDir(e: CustomEvent<{ prefix: string }>) {
		openFileView({ ...(fileView!), panelMode: 'dir', dirPrefix: e.detail.prefix });
	}

	function handleLineSelect(e: CustomEvent<{ selection: LineSelection }>) {
		if (fileView) fileView = { ...fileView, selection: e.detail.selection };
		syncHash();
	}

	function handleTreeToggle() {
		showTree = !showTree;
	}

	async function handleBack() {
		fileView = null;
		pushState();
		await tick();
		if (mainContent) mainContent.scrollTop = savedScrollTop;
	}

	// ── Command palette ──────────────────────────────────────────────────────────

	function handlePaletteSelect(e: CustomEvent<{ source: string; path: string; archivePath: string | null; kind: string }>) {
		const file = FilePath.fromParts(e.detail.path, e.detail.archivePath);
		if (e.detail.kind === 'archive') {
			openFileView({ source: e.detail.source, file, selection: [], panelMode: 'dir', dirPrefix: file.full + '::' });
		} else {
			openFileView({ source: e.detail.source, file, selection: [], panelMode: 'file', dirPrefix: '' });
		}
	}

	// ── Sidebar resize ───────────────────────────────────────────────────────────

	function onResizeStart(e: MouseEvent) {
		const startX = e.clientX;
		const startWidth = sidebarWidth;
		function onMove(ev: MouseEvent) {
			sidebarWidth = Math.min(600, Math.max(120, startWidth + ev.clientX - startX));
		}
		function onUp() {
			document.removeEventListener('mousemove', onMove);
			document.removeEventListener('mouseup', onUp);
			profile.update((p) => ({ ...p, sidebarWidth }));
		}
		document.addEventListener('mousemove', onMove);
		document.addEventListener('mouseup', onUp);
	}

	function onResizeKeydown(e: KeyboardEvent) {
		const step = e.shiftKey ? 32 : 8;
		if (e.key === 'ArrowRight') {
			e.preventDefault();
			sidebarWidth = Math.min(600, sidebarWidth + step);
			profile.update((p) => ({ ...p, sidebarWidth }));
		} else if (e.key === 'ArrowLeft') {
			e.preventDefault();
			sidebarWidth = Math.max(120, sidebarWidth - step);
			profile.update((p) => ({ ...p, sidebarWidth }));
		}
	}

	// ── Derived ──────────────────────────────────────────────────────────────────

	$: sourceNames = sources.map((s) => s.name);
	$: paletteSources = selectedSources.length ? selectedSources : fileView?.source ? [fileView.source] : sourceNames;
</script>

<div class="page-layout" class:has-sidebar={showTree} class:file-view={fileView !== null}>
	{#if showTree}
		<aside class="global-sidebar" style="width: {sidebarWidth}px">
			<MultiSourceTree
				sources={sourceNames}
				activeSource={fileView?.source ?? null}
				activePath={fileView?.file.full ?? null}
				on:open={handleOpenFileFromTree}
			/>
		</aside>
		<button
			class="resize-handle"
			type="button"
			aria-label="Resize sidebar"
			on:mousedown={onResizeStart}
			on:keydown={onResizeKeydown}
		/>
	{/if}

	<div class="main-content" bind:this={mainContent}>
		{#if fileView !== null}
			<FileView
				{fileView}
				{showTree}
				{query}
				{scope}
				{matchType}
				{searching}
				sources={sourceNames}
				{selectedSources}
				{selectedKinds}
				{caseSensitive}
				dateFrom={dateFromStr}
				dateTo={dateToStr}
				on:back={handleBack}
				on:search={handleSearch}
				on:filterChange={handleFilterChange}
				on:treeToggle={handleTreeToggle}
				on:openFileFromTree={handleOpenFileFromTree}
				on:openDirFile={handleOpenDirFile}
				on:openDir={handleOpenDir}
				on:lineselect={handleLineSelect}
			/>
		{:else}
			<SearchView
				bind:this={searchView}
				{query}
				{scope}
				{matchType}
				{searching}
				sources={sourceNames}
				{selectedSources}
				{selectedKinds}
				{caseSensitive}
				dateFrom={dateFromStr}
				dateTo={dateToStr}
				{results}
				{totalResults}
				{searchError}
				{searchId}
				{showTree}
				filterDateFrom={effectiveDateFrom}
				filterDateTo={effectiveDateTo}
				nlpDateLabel={nlpResult?.dateLabel}
				nlpDetectedPhrase={nlpResult?.detectedPhrase}
				nlpConflict={!!nlpResult?.dateLabel && (dateFromTs != null || dateToTs != null)}
				on:search={handleSearch}
				on:filterChange={handleFilterChange}
				on:clearNlpDate={handleClearNlpDate}
				on:open={openFile}
				on:treeToggle={handleTreeToggle}
				{resultsStale}
				{deletedPaths}
				on:refreshResults={() => { doSearch(query, selectedSources); }}
				on:dismissStale={() => { resultsStale = false; }}
			/>
			<div bind:this={sentinel}></div>
			{#if loadingMore}
				<div class="load-row">
					<div class="spinner">
						<svg viewBox="0 0 24 24" fill="none">
							<circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="3" opacity="0.25"/>
							<path d="M12 2a10 10 0 0 1 10 10" stroke="currentColor" stroke-width="3" stroke-linecap="round"/>
						</svg>
					</div>
					<span>Loading more results…</span>
				</div>
			{/if}
		{/if}
	</div>
</div>

<CommandPalette
	open={showPalette}
	sources={paletteSources}
	totalSourceCount={sourceNames.length}
	on:select={handlePaletteSelect}
	on:close={() => (showPalette = false)}
/>



{#if showTokenSetup}
	<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
	<div class="token-overlay" on:click|self={() => {}}>
		<div class="token-dialog">
			<h2>Connect to find-server</h2>
			<p>Enter the bearer token from your <code>server.toml</code> to connect.</p>
			<input
				type="password"
				placeholder="Paste your token here"
				bind:value={tokenInput}
				on:keydown={(e) => e.key === 'Enter' && saveToken()}
			/>
			<button on:click={saveToken} disabled={!tokenInput.trim()}>Connect</button>
		</div>
	</div>
{/if}

<style>
	.page-layout {
		display: flex;
		flex-direction: row;
		height: 100vh;
		overflow: hidden;
	}

	.global-sidebar {
		flex-shrink: 0;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		background: var(--bg-secondary);
		border-right: 1px solid var(--border);
	}

	.resize-handle {
		width: 4px;
		flex-shrink: 0;
		cursor: col-resize;
		background: var(--border);
		border: none;
		padding: 0;
		transition: background 0.15s;
	}

	.resize-handle:focus-visible {
		outline: 2px solid var(--accent, #58a6ff);
		outline-offset: 0;
	}

	.resize-handle:hover {
		background: var(--accent, #58a6ff);
	}

	.main-content {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		overflow-y: auto;
	}

	.page-layout.file-view .main-content {
		overflow: hidden;
	}

	.load-row {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 10px;
		height: 56px;
		color: var(--text-muted);
		font-size: 14px;
		padding: 0 16px;
	}

	.load-row .spinner {
		width: 16px;
		height: 16px;
		flex-shrink: 0;
	}

	.load-row .spinner svg {
		width: 100%;
		height: 100%;
		color: var(--accent);
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}

	.token-overlay {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.6);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 1000;
	}

	.token-dialog {
		background: var(--bg, #1e1e1e);
		border: 1px solid var(--border, #333);
		border-radius: 8px;
		padding: 32px;
		width: min(420px, 90vw);
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.token-dialog h2 {
		margin: 0;
		font-size: 18px;
		font-weight: 600;
	}

	.token-dialog p {
		margin: 0;
		font-size: 14px;
		color: var(--text-muted, #999);
		line-height: 1.5;
	}

	.token-dialog input {
		width: 100%;
		padding: 10px 12px;
		border: 1px solid var(--border, #333);
		border-radius: 6px;
		background: var(--bg-input, #2a2a2a);
		color: inherit;
		font-size: 14px;
		font-family: monospace;
		box-sizing: border-box;
	}

	.token-dialog input:focus {
		outline: 2px solid var(--accent, #4a9eff);
		outline-offset: -1px;
	}

	.token-dialog button {
		align-self: flex-end;
		padding: 8px 20px;
		background: var(--accent, #4a9eff);
		color: #fff;
		border: none;
		border-radius: 6px;
		font-size: 14px;
		font-weight: 500;
		cursor: pointer;
	}

	.token-dialog button:disabled {
		opacity: 0.4;
		cursor: not-allowed;
	}
</style>

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
	import { contextWindow } from '$lib/settingsStore';
	import { formatHash } from '$lib/lineSelection';
	import type { LineSelection } from '$lib/lineSelection';
	import { FilePath } from '$lib/filePath';
	import { buildUrl, restoreFromParams, serializeState, deserializeState } from '$lib/appState';
	import type { AppState, SerializedAppState } from '$lib/appState';
	import { profile } from '$lib/profile';

	// SvelteKit passes params to every layout/page component. Declare it to avoid
	// the runtime "unknown prop" warning. Assigned to _params to signal that it
	// is intentionally unused (access via $page.params if ever needed).
	export let params: Record<string, string>;
	const _params = params;

	// ── State ──────────────────────────────────────────────────────────────────

	let view: 'results' | 'file' = 'results';
	let query = '';
	let mode = 'fuzzy';

	let sources: SourceInfo[] = [];
	let selectedSources: string[] = [];
	// ISO date strings bound to the AdvancedSearch inputs (propagated back for controlled state).
	let dateFromStr = '';
	let dateToStr = '';
	// Unix timestamp equivalents sent to the API (undefined = no filter).
	let dateFromTs: number | undefined;
	let dateToTs: number | undefined;

	let results: SearchResult[] = [];
	let totalResults = 0;
	let searching = false;
	let searchError: string | null = null;
	let searchId = 0;

	let fileSource = '';
	let currentFile: FilePath | null = null;
	let fileSelection: LineSelection = [];
	let panelMode: 'file' | 'dir' = 'file';
	let currentDirPrefix = '';
	let showTree = true;
	let showPalette = false;

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
		} catch { /* silent */ }
	}

	// ── History ─────────────────────────────────────────────────────────────────

	function captureState(): AppState {
		return { view, query, mode, selectedSources, fileSource, currentFile, fileSelection, panelMode, currentDirPrefix };
	}

	function pushState() {
		const s = captureState();
		svelteKitPushState(buildUrl(s) + formatHash(fileSelection), serializeState(s));
	}

	function syncHash() {
		const hash = formatHash(fileSelection);
		const base = location.pathname + location.search;
		svelteKitReplaceState(hash ? base + hash : base, get(page).state);
	}

	function applyState(s: AppState) {
		view = s.view;
		query = s.query;
		mode = s.mode;
		selectedSources = s.selectedSources;
		fileSource = s.fileSource;
		currentFile = s.currentFile;
		fileSelection = s.fileSelection;
		panelMode = s.panelMode;
		currentDirPrefix = s.currentDirPrefix;
		if (s.query) doSearch(s.query, s.mode, s.selectedSources, false);
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
		window.addEventListener('scroll', checkScroll, { passive: true });
		return () => {
			window.removeEventListener('keydown', handleKeydown, { capture: true });
			window.removeEventListener('scroll', checkScroll);
		};
	});

	// ── Load more ───────────────────────────────────────────────────────────────

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
		if (loadingMore || noMoreResults || view !== 'results' || query.trim().length < 3) return;
		if (isNearBottom()) triggerLoad();
	}

	async function triggerLoad() {
		if (loadingMore || noMoreResults || query.trim().length < 3) return;
		loadingMore = true;
		try {
			const resp = await search({ q: query, mode, sources: selectedSources, limit: 50, offset: loadOffset, dateFrom: dateFromTs, dateTo: dateToTs });
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
				const seen = new Set(results.map(r => `${r.source}:${r.path}:${r.archive_path ?? ''}:${r.line_number}`));
				const fresh = resp.results.filter(r => !seen.has(`${r.source}:${r.path}:${r.archive_path ?? ''}:${r.line_number}`));
				results = [...results, ...fresh];
				totalResults = resp.total;
				// Advance by full server response, not fresh.length — see loadOffset comment above.
				loadOffset += resp.results.length;
			}
			await tick();
		} catch { /* silent */ }
		loadingMore = false;
		// getBoundingClientRect() forces layout, so this is accurate after tick().
		// If sentinel is still near the bottom, keep filling the viewport.
		if (isNearBottom()) triggerLoad();
	}

	// ── Search ──────────────────────────────────────────────────────────────────

	async function doSearch(q: string, m: string, srcs: string[], push = true) {
		if (q.trim().length < 3) {
			results = []; totalResults = 0; noMoreResults = false; loadOffset = 0; searchError = null;
			return;
		}
		searching = true;
		searchError = null;
		searchId += 1;
		noMoreResults = false;
		loadOffset = 0;
		if (push) {
			pushState();
			window.scrollTo(0, 0);
		}
		try {
			const resp = await search({ q, mode: m, sources: srcs, limit: 50, offset: 0, dateFrom: dateFromTs, dateTo: dateToTs });
			results = resp.results;
			totalResults = resp.total;
			loadOffset = resp.results.length; // server cursor starts after page 0
			if (resp.results.length === 0) noMoreResults = true;
			if (push) view = 'results';
		} catch (e) {
			searchError = String(e);
			results = []; totalResults = 0; noMoreResults = true; loadOffset = 0;
			if (push) view = 'results';
		} finally {
			searching = false;
		}
		// Auto-fill viewport if the first page doesn't reach the scroll threshold.
		await tick();
		if (isNearBottom()) triggerLoad();
	}

	// ── Search event handlers ────────────────────────────────────────────────────

	function handleSearch(e: CustomEvent<{ query: string; mode: string }>) {
		query = e.detail.query;
		mode = e.detail.mode;
		doSearch(query, mode, selectedSources);
	}

	function handleFilterChange(e: CustomEvent<{ sources: string[]; dateFrom?: number; dateTo?: number }>) {
		selectedSources = e.detail.sources;
		dateFromTs = e.detail.dateFrom;
		dateToTs = e.detail.dateTo;
		// Keep ISO strings in sync so AdvancedSearch inputs remain controlled.
		dateFromStr = dateFromTs != null ? new Date(dateFromTs * 1000).toISOString().slice(0, 10) : '';
		dateToStr = dateToTs != null ? new Date(dateToTs * 1000).toISOString().slice(0, 10) : '';
		if (query.trim()) doSearch(query, mode, selectedSources);
	}

	// ── File viewer event handlers ───────────────────────────────────────────────

	function openFile(e: CustomEvent<SearchResult>) {
		const r = e.detail;
		fileSource = r.source;
		currentFile = FilePath.fromParts(r.path, r.archive_path ?? null);
		const extraLines = (r.extra_matches ?? [])
			.map((m) => m.line_number)
			.filter((n) => n > 0 && n !== r.line_number);
		fileSelection = r.line_number
			? [r.line_number, ...extraLines]
			: extraLines.length ? extraLines : [];
		panelMode = 'file';
		view = 'file';
		pushState();
	}

	function handleOpenFileFromTree(e: CustomEvent<{ source: string; path: string; kind: string; archivePath?: string; showAsDirectory?: boolean }>) {
		fileSource = e.detail.source;
		currentFile = FilePath.fromParts(e.detail.path, e.detail.archivePath ?? null);
		fileSelection = [];
		if (e.detail.showAsDirectory) {
			panelMode = 'dir';
			currentDirPrefix = currentFile.full + '::';
		} else {
			panelMode = 'file';
		}
		view = 'file';
		pushState();
	}

	function handleOpenDirFile(e: CustomEvent<{ source: string; path: string; kind: string; archivePath?: string }>) {
		currentFile = FilePath.fromParts(e.detail.path, e.detail.archivePath ?? null);
		fileSelection = [];
		panelMode = 'file';
		pushState();
	}

	function handleOpenDir(e: CustomEvent<{ prefix: string }>) {
		currentDirPrefix = e.detail.prefix;
		panelMode = 'dir';
		pushState();
	}

	function handleLineSelect(e: CustomEvent<{ selection: LineSelection }>) {
		fileSelection = e.detail.selection;
		syncHash();
	}

	function handleTreeToggle() {
		showTree = !showTree;
	}

	function handleBack() {
		view = 'results';
		pushState();
	}

	// ── Command palette ──────────────────────────────────────────────────────────

	function handlePaletteSelect(e: CustomEvent<{ source: string; path: string; archivePath: string | null; kind: string }>) {
		fileSource = e.detail.source;
		fileSelection = [];
		view = 'file';
		currentFile = FilePath.fromParts(e.detail.path, e.detail.archivePath);
		if (e.detail.kind === 'archive') {
			panelMode = 'dir';
			currentDirPrefix = currentFile.full + '::';
		} else {
			panelMode = 'file';
		}
		pushState();
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
	$: serverBaseUrls = Object.fromEntries(sources.filter((s) => s.base_url != null).map((s) => [s.name, s.base_url as string]));
	$: paletteSources = selectedSources.length ? selectedSources : fileSource ? [fileSource] : sourceNames;
	$: fileBaseUrl = $profile.sourceBaseUrls?.[fileSource] ?? serverBaseUrls[fileSource] ?? null;
</script>

<div class="page-layout" class:has-sidebar={showTree} class:file-view={view === 'file'}>
	{#if showTree}
		<aside class="global-sidebar" style="width: {sidebarWidth}px">
			<MultiSourceTree
				sources={sourceNames}
				activeSource={view === 'file' ? fileSource : null}
				activePath={view === 'file' ? (currentFile?.full ?? null) : null}
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

	<div class="main-content">
		{#if view === 'file'}
			<FileView
				{fileSource}
				{currentFile}
				{fileSelection}
				{panelMode}
				{currentDirPrefix}
				{showTree}
				baseUrl={fileBaseUrl}
				{query}
				{mode}
				{searching}
				sources={sourceNames}
				{selectedSources}
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
				{query}
				{mode}
				{searching}
				sources={sourceNames}
				{selectedSources}
				dateFrom={dateFromStr}
				dateTo={dateToStr}
				{results}
				{totalResults}
				{searchError}
				{searchId}
				{showTree}
				on:search={handleSearch}
				on:filterChange={handleFilterChange}
				on:open={openFile}
				on:treeToggle={handleTreeToggle}
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
		min-height: 100vh;
	}

	.page-layout.file-view {
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
	}

	.page-layout.file-view .main-content {
		height: 100vh;
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

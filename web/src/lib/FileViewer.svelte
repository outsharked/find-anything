<script lang="ts">
	import { createEventDispatcher, onMount, tick } from 'svelte';
	import { getFile } from '$lib/api';
	import { fileViewPageSize } from '$lib/settingsStore';
	import { highlightFile } from '$lib/highlight';
	import DirListing from './DirListing.svelte';
	import ImageViewer from './ImageViewer.svelte';
	import MarkdownViewer from './MarkdownViewer.svelte';
	import CodeViewer from './CodeViewer.svelte';
	import PdfViewer from './PdfViewer.svelte';
	import VideoViewer from './VideoViewer.svelte';
	import FileStatusBanner from './FileStatusBanner.svelte';
	import {
		type LineSelection,
		firstLine,
	} from '$lib/lineSelection';
	import { profile } from '$lib/profile';
	import { parseImageDimensions } from '$lib/imageMeta';
	import { marked } from 'marked';
	import { maxMarkdownRenderKb } from '$lib/settingsStore';
	import { liveEvent } from '$lib/liveUpdates';

	export let source: string;
	export let path: string;
	export let archivePath: string | null = null;
	export let selection: LineSelection = [];
	/** Whether to default to the original (rendered) view when the file is opened.
	 * True for tree/dir/palette opens; false for search-result opens with context. */
	export let preferOriginal: boolean = false;

	const dispatch = createEventDispatcher<{
		lineselect: { selection: LineSelection };
		open: { source: string; path: string; kind: string; archivePath?: string };
		navigateDir: { prefix: string };
		navigate: { path: string };
	}>();

	let loading = true;
	let error: string | null = null;
	let contentUnavailable = false;
	let highlightedCode = '';
	/** Maps 0-based render index → line_number */
	let lineOffsets: number[] = [];
	let mtime: number | null = null;
	let size: number | null = null;
	let fileKind: string | null = null;
	let rawContent = '';
	let isEncrypted = false;
	let indexingError: string | null = null;
	/** Metadata lines (line_number === 0, excluding the path line itself). */
	let metaLines: { content: string }[] = [];
	/** Paths of duplicate/canonical copies of this file (dedup aliases). */
	let duplicatePaths: string[] = [];

	// Original file view
	let showOriginal = false;
	// Track previous preferOriginal to detect changes after the component is mounted
	// (e.g. same file re-opened from a different entry point without remounting).
	let _prevPreferOriginal = preferOriginal;
	$: if (preferOriginal !== _prevPreferOriginal) {
		_prevPreferOriginal = preferOriginal;
		if (fileKind !== null) {
			showOriginal = fileKind === 'image' || fileKind === 'video' || (fileKind === 'pdf' && !isEncrypted && preferOriginal);
		}
	}
	// For images: false = split view (image + metadata side-by-side), true = full-width image
	let imageFullWidth = false;

	// Parsed image dimensions for the aspect-ratio loading placeholder.
	$: imgDims = parseImageDimensions(metaLines);
	$: placeholderStyle = imgDims
		? `width: min(${imgDims.width}px, 100%); aspect-ratio: ${imgDims.width} / ${imgDims.height}; max-height: min(${imgDims.height}px, 100%); min-height: 0;`
		: '';
	// archivePath is set when this file is a member of an archive.
	// path is always the outer (real) file path — it never contains '::'.
	$: isArchiveMember = archivePath !== null;

	// For inline archive browsing: tracks the current dir prefix within the archive.
	let archivePrefix = '';
	$: if (fileKind === 'archive' && !archivePath && path) archivePrefix = path + '::';
	// Download/stream URL for the outer file (used for download link and PDF iframe).
	$: rawUrl = `/api/v1/raw?source=${encodeURIComponent(source)}&path=${encodeURIComponent(path)}`;
	// For inline image display, use the composite path for archive members so the
	// server extracts the member from the outer ZIP.
	$: rawInlinePath = archivePath ? `${path}::${archivePath}` : path;
	// Images, PDFs, and videos can be shown inline, including archive members.
	// The server extracts archive members from the outer ZIP via composite paths.
	$: canViewInline = fileKind === 'image' || (fileKind === 'pdf' && !isEncrypted) || fileKind === 'video';
	// For images the browser can't render natively, request server-side PNG conversion.
	// Check the member's own extension for archive members.
	const BROWSER_IMAGE_EXTS = new Set(['jpg','jpeg','png','gif','webp','svg','svgz','avif','bmp','ico']);
	$: imageExtPath = archivePath ?? path;
	$: needsConversion = fileKind === 'image' && !BROWSER_IMAGE_EXTS.has((imageExtPath.split('.').pop() ?? '').toLowerCase());
	$: rawInlineUrl = needsConversion
		? `/api/v1/raw?source=${encodeURIComponent(source)}&path=${encodeURIComponent(rawInlinePath)}&convert=png`
		: `/api/v1/raw?source=${encodeURIComponent(source)}&path=${encodeURIComponent(rawInlinePath)}`;
	$: fileName = path.split('/').pop() ?? path;
	// Member download: the server can extract members from ZIP archives up to a configured
	// nesting depth (window.find_anything_config.download_zip_member_levels).
	// TAR, 7z, etc. are not supported — fall back to downloading the outer archive.
	const downloadZipMemberLevels: number =
		(typeof window !== 'undefined' && window.find_anything_config?.download_zip_member_levels) || 1;
	$: outerExt = (path.split('.').pop() ?? '').toLowerCase();
	$: canDownloadMember = (() => {
		if (!isArchiveMember || outerExt !== 'zip') return false;
		const parts = (archivePath ?? '').split('::');
		// Every intermediate segment (all but the last) must also be a ZIP.
		for (let i = 0; i < parts.length - 1; i++) {
			if ((parts[i].split('.').pop() ?? '').toLowerCase() !== 'zip') return false;
		}
		// Total nesting depth = number of '::' in the composite path.
		return parts.length <= downloadZipMemberLevels;
	})();
	$: memberFileName = archivePath ? (archivePath.split('/').pop()?.split('::').pop() ?? archivePath) : '';

	// Detect if file is markdown
	$: isMarkdown = path.endsWith('.md') || path.endsWith('.markdown');

	// Word wrap preference (default: false for code, true for text files)
	$: wordWrap = $profile.wordWrap ?? false;

	// Markdown format preference
	$: markdownFormat = $profile.markdownFormat ?? false;

	// True when the markdown content exceeds the server-configured size cap.
	$: markdownTooLarge = isMarkdown && rawContent.length > $maxMarkdownRenderKb * 1024;

	// Render markdown to HTML (skipped when file exceeds size cap).
	$: renderedMarkdown = markdownFormat && isMarkdown && !markdownTooLarge
		? marked.parse(rawContent, { gfm: true, breaks: true })
		: '';

	function toggleWordWrap() {
		$profile.wordWrap = !wordWrap;
	}

	function toggleMarkdownFormat() {
		$profile.markdownFormat = !markdownFormat;
	}

	function formatSize(bytes: number | null): string {
		if (bytes === null) return '';
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
		return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
	}

	function formatDate(timestamp: number | null): string {
		if (timestamp === null) return '';
		const date = new Date(timestamp * 1000);
		return date.toLocaleString();
	}

	// ── Paged loading state ──────────────────────────────────────────────────────

	let pagedMode = false;
	/** Accumulated content lines (strings) across all loaded pages. */
	let allContentLines: string[] = [];
	/** Accumulated line offsets (1-based actual line_numbers) for allContentLines. */
	let allLineOffsets: number[] = [];
	/** True total content line count as reported by the server. */
	let totalLines = 0;
	/** Next content-line index to fetch in the forward direction. */
	let forwardOffset = 0;
	/** Start of the earliest page loaded (for backward loading). */
	let backwardOffset = 0;
	let loadingForward = false;
	let loadingBackward = false;
	let noMoreForward = false;
	let noMoreBackward = true;

	/** Reference to the scrollable .code-container element. */
	let codeContainer: HTMLElement;

	function isNearBottom(): boolean {
		if (!codeContainer) return false;
		return codeContainer.scrollHeight - codeContainer.scrollTop - codeContainer.clientHeight < 600;
	}

	function isNearTop(): boolean {
		if (!codeContainer) return false;
		return codeContainer.scrollTop < 300;
	}

	function handleScroll() {
		if (!pagedMode) return;
		if (!loadingForward && !noMoreForward && isNearBottom()) loadForward();
		if (!loadingBackward && !noMoreBackward && isNearTop()) loadBackward();
	}

	/** Rebuild rawContent / highlightedCode / lineOffsets from accumulated lines. */
	function updateCodeState() {
		lineOffsets = allLineOffsets;
		rawContent = allContentLines.join('\n');
		highlightedCode = highlightFile(allContentLines, path);
	}

	function applyFileData(data: import('$lib/api').FileResponse, isInitial: boolean) {
		contentUnavailable = data.content_unavailable ?? false;
		if (contentUnavailable) return;
		error = null;

		// Separate metadata into: current path (skip), metadata strings, duplicate paths.
		// Metadata strings start with '['. Other strings are file paths (dedup aliases).
		const compositePath = archivePath ? `${path}::${archivePath}` : path;
		metaLines = [];
		duplicatePaths = [];
		for (const s of data.metadata) {
			if (s === compositePath) continue;
			if (s.startsWith('[')) {
				metaLines.push({ content: s });
			} else {
				duplicatePaths.push(s);
			}
		}
		// Also include duplicate_paths from the dedicated field (schema v3).
		for (const dup of (data.duplicate_paths ?? [])) {
			if (!duplicatePaths.includes(dup)) duplicatePaths.push(dup);
		}

		lineOffsets = data.line_offsets && data.line_offsets.length > 0
			? data.line_offsets
			: data.lines.map((_, i) => i + 1);
		rawContent = data.lines.join('\n');
		highlightedCode = highlightFile(data.lines, path);
		mtime = data.mtime;
		size = data.size;
		fileKind = data.file_kind ?? null;
		indexingError = data.indexing_error ?? null;
		isEncrypted = fileKind === 'pdf' && data.lines.length === 1 && data.lines[0] === 'Content encrypted';
		if (isInitial) {
			showOriginal = fileKind === 'image' || fileKind === 'video' || (fileKind === 'pdf' && !isEncrypted && preferOriginal);
			imageFullWidth = false;
		}
	}

	/** Apply file-level metadata from the initial response (for paged mode). */
	function applyFileMeta(data: import('$lib/api').FileResponse, isInitial: boolean) {
		mtime = data.mtime;
		size = data.size;
		fileKind = data.file_kind ?? null;
		indexingError = data.indexing_error ?? null;
		const compositePath = archivePath ? `${path}::${archivePath}` : path;
		metaLines = [];
		duplicatePaths = [];
		for (const s of data.metadata) {
			if (s === compositePath) continue;
			if (s.startsWith('[')) {
				metaLines.push({ content: s });
			} else {
				duplicatePaths.push(s);
			}
		}
		// Also include duplicate_paths from the dedicated field (schema v3).
		for (const dup of (data.duplicate_paths ?? [])) {
			if (!duplicatePaths.includes(dup)) duplicatePaths.push(dup);
		}
		if (isInitial) {
			isEncrypted = fileKind === 'pdf' && data.lines.length === 1 && data.lines[0] === 'Content encrypted';
			showOriginal = fileKind === 'image' || fileKind === 'video' || (fileKind === 'pdf' && !isEncrypted && preferOriginal);
			imageFullWidth = false;
		}
	}

	async function loadFile(isInitial: boolean) {
		loading = true;
		pagedMode = false;
		allContentLines = [];
		allLineOffsets = [];
		noMoreForward = false;
		noMoreBackward = true;

		try {
			const pageSize = $fileViewPageSize;
			const firstLn = firstLine(selection);
			// Anchor the first page so the selected line is visible.
			const anchorOffset = (firstLn !== null && pageSize > 0)
				? Math.max(0, Math.floor((firstLn - 1) / pageSize) * pageSize)
				: 0;

			const data = await getFile(
				source, path, archivePath ?? undefined,
				pageSize > 0 ? anchorOffset : undefined,
				pageSize > 0 ? pageSize : undefined,
			);

			contentUnavailable = data.content_unavailable ?? false;
			if (contentUnavailable) return;
			error = null;

			if (pageSize > 0 && data.total_lines > pageSize) {
				// Paged mode.
				pagedMode = true;
				applyFileMeta(data, isInitial);

				const pageOffsets = data.line_offsets && data.line_offsets.length > 0
					? data.line_offsets
					: data.lines.map((_, i) => anchorOffset + i + 1);
				allContentLines = [...data.lines];
				allLineOffsets = pageOffsets;
				totalLines = data.total_lines;
				forwardOffset = anchorOffset + data.lines.length;
				backwardOffset = anchorOffset;
				noMoreForward = forwardOffset >= totalLines;
				noMoreBackward = anchorOffset === 0;
				updateCodeState();
			} else {
				// Single-page (full file) mode — identical to previous behaviour.
				applyFileData(data, isInitial);
			}
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}

		if (isInitial) {
			const ln = firstLine(selection);
			if (ln !== null) {
				await tick();
				scrollToLine(ln);
			}
		}
	}

	async function loadForward() {
		if (loadingForward || noMoreForward) return;
		loadingForward = true;
		try {
			const pageSize = $fileViewPageSize;
			const data = await getFile(source, path, archivePath ?? undefined, forwardOffset, pageSize);
			const pageOffsets = data.line_offsets && data.line_offsets.length > 0
				? data.line_offsets
				: data.lines.map((_, i) => forwardOffset + i + 1);
			allContentLines = [...allContentLines, ...data.lines];
			allLineOffsets = [...allLineOffsets, ...pageOffsets];
			forwardOffset += data.lines.length;
			noMoreForward = forwardOffset >= totalLines;
			updateCodeState();
			await tick();
		} catch { /* silent — user can scroll again to retry */ }
		loadingForward = false;
		if (isNearBottom() && !noMoreForward) loadForward();
	}

	async function loadBackward() {
		if (loadingBackward || noMoreBackward || !codeContainer) return;
		loadingBackward = true;
		try {
			const pageSize = $fileViewPageSize;
			const prevOffset = Math.max(0, backwardOffset - pageSize);
			const limit = backwardOffset - prevOffset;
			const data = await getFile(source, path, archivePath ?? undefined, prevOffset, limit);
			const pageOffsets = data.line_offsets && data.line_offsets.length > 0
				? data.line_offsets
				: data.lines.map((_, i) => prevOffset + i + 1);

			// Preserve scroll position when prepending.
			const oldScrollHeight = codeContainer.scrollHeight;
			const oldScrollTop = codeContainer.scrollTop;

			allContentLines = [...data.lines, ...allContentLines];
			allLineOffsets = [...pageOffsets, ...allLineOffsets];
			backwardOffset = prevOffset;
			noMoreBackward = prevOffset === 0;
			updateCodeState();

			await tick();
			codeContainer.scrollTop = oldScrollTop + (codeContainer.scrollHeight - oldScrollHeight);
		} catch { /* silent */ }
		loadingBackward = false;
	}

	onMount(async () => {
		await loadFile(true);
	});

	function openDuplicate(dupPath: string) {
		const i = dupPath.indexOf('::');
		const outerPath = i >= 0 ? dupPath.slice(0, i) : dupPath;
		const archivePath = i >= 0 ? dupPath.slice(i + 2) : undefined;
		dispatch('open', { source, path: outerPath, kind: 'unknown', archivePath });
	}

	function scrollToLine(ln: number) {
		const el = document.getElementById(`line-${ln}`);
		if (el) el.scrollIntoView({ behavior: 'smooth', block: 'center' });
	}

	$: codeLines = highlightedCode ? highlightedCode.split('\n') : [];

	// Live update state
	type FileState = 'normal' | 'deleted' | 'renamed' | 'modified';
	let fileState: FileState = 'normal';
	let renamedTo: string | null = null;

	// The outer path to watch for live events. For archive members, events fire
	// for the outer archive file, not the inner member.
	$: watchPath = path;

	// Track the last handled event by reference so that clicking Reload doesn't
	// immediately re-show the banner: after reload completes loading=false
	// re-triggers this block, but the event hasn't changed so we skip it.
	let lastHandledLiveEvent: typeof $liveEvent | null = null;

	$: if ($liveEvent && !loading && $liveEvent !== lastHandledLiveEvent &&
	       $liveEvent.source === source && $liveEvent.path === watchPath) {
		lastHandledLiveEvent = $liveEvent;
		const ev = $liveEvent;
		if (ev.action === 'deleted') {
			fileState = 'deleted';
		} else if (ev.action === 'modified') {
			if (fileState !== 'deleted') fileState = 'modified';
		} else if (ev.action === 'renamed') {
			fileState = 'renamed';
			renamedTo = ev.new_path ?? null;
		}
	}

	async function reload() {
		fileState = 'normal';
		renamedTo = null;
		await loadFile(false);
	}
</script>

<div class="file-viewer">
	{#if loading}
		<div class="status">Loading…</div>
	{:else if contentUnavailable}
		<div class="status">Content not yet available. <button class="inline-link" on:click={reload}>Reload</button></div>
	{:else if error}
		<div class="status error">{error}</div>
	{:else}
		<FileStatusBanner
			{fileState}
			{renamedTo}
			{indexingError}
			on:navigate={(e) => dispatch('navigate', e.detail)}
			on:dismiss={() => fileState = 'normal'}
			on:reload={reload}
		/>
		<div class="toolbar">
			<button class="toolbar-btn" on:click={toggleWordWrap} title="Toggle word wrap">
				{wordWrap ? '⊟' : '⊞'} Wrap
			</button>
			{#if isMarkdown && !markdownTooLarge}
				<button class="toolbar-btn" on:click={toggleMarkdownFormat} title="Toggle markdown formatting">
					{markdownFormat ? 'Plain' : 'Formatted'}
				</button>
			{/if}
			{#if canViewInline && (fileKind === 'image' || fileKind === 'pdf' || fileKind === 'video')}
				{#if fileKind === 'image'}
					{#if imageFullWidth}
						<button class="toolbar-btn" on:click={() => imageFullWidth = false}>View Split</button>
					{:else}
						<button class="toolbar-btn" on:click={() => imageFullWidth = true}>View Extracted</button>
					{/if}
				{:else}
					<button class="toolbar-btn" on:click={() => showOriginal = !showOriginal}
							title="Toggle original file view">
						{showOriginal ? 'View Extracted' : 'View Original'}
					</button>
				{/if}
			{/if}
			{#if canDownloadMember}
				<a href={rawInlineUrl} download={memberFileName} class="toolbar-btn">Download</a>
			{:else}
				<a href={rawUrl} download={fileName} class="toolbar-btn">
					{isArchiveMember || fileKind === 'archive' ? 'Download Archive' : 'Download'}
				</a>
			{/if}
			<div class="metadata">
				{#if fileKind && fileKind !== 'raw'}
					<span class="meta-item kind-badge" title="File type">{fileKind}</span>
				{/if}
				{#if size !== null}
					<span class="meta-item" title="File size">{formatSize(size)}</span>
				{/if}
				{#if mtime !== null}
					<span class="meta-item" title="Last modified">{formatDate(mtime)}</span>
				{/if}
			</div>
		</div>
		{#if showOriginal && canViewInline}
			{#if fileKind === 'image'}
				<ImageViewer
					src={rawInlineUrl}
					{path}
					fullWidth={imageFullWidth}
					{placeholderStyle}
					{metaLines}
					{duplicatePaths}
					on:openDuplicate={(e) => openDuplicate(e.detail.path)}
				/>
			{:else if fileKind === 'video'}
				<VideoViewer src={rawInlineUrl} />
			{:else}
				<!-- PDF / other inline kind -->
				<PdfViewer src={rawInlineUrl} />
			{/if}
		{:else}
			<!-- Extracted text / code view -->
			<div class="code-container" bind:this={codeContainer} on:scroll={handleScroll}>
				{#if pagedMode && !noMoreBackward}
					<div class="load-sentinel">
						{#if loadingBackward}
							<span class="sentinel-msg">Loading earlier lines…</span>
						{:else}
							<button class="sentinel-btn" on:click={loadBackward}>Load earlier lines</button>
						{/if}
					</div>
				{/if}
				{#if isEncrypted}
					<div class="encrypted-notice">🔒 This PDF is password-protected and cannot be displayed.</div>
				{/if}
				{#if metaLines.length > 0 || duplicatePaths.length > 0}
					<div class="meta-panel">
						{#each duplicatePaths as dup}
							<div class="meta-row duplicate-row">
								<span class="duplicate-label">DUPLICATE:</span>
								<button class="duplicate-link" on:click={() => openDuplicate(dup)}>{dup}</button>
							</div>
						{/each}
						{#each metaLines as meta}
							<div class="meta-row">{meta.content}</div>
						{/each}
					</div>
				{/if}
				{#if markdownTooLarge && markdownFormat}
					<div class="no-content">File too large to render as markdown ({Math.round(rawContent.length / 1024)} KB &gt; {$maxMarkdownRenderKb} KB limit). Showing plain text.</div>
				{/if}
				{#if markdownFormat && isMarkdown && !markdownTooLarge}
					<MarkdownViewer rendered={String(renderedMarkdown)} />
				{:else if codeLines.length === 0 && metaLines.length === 0 && duplicatePaths.length === 0 && fileKind === 'archive' && !archivePath}
					<!-- Archive root: show member listing inline -->
					<DirListing
						source={source}
						prefix={archivePrefix}
						on:openFile={(e) => {
							const p = e.detail.path;
							const i = p.indexOf('::');
							const outerPath = i >= 0 ? p.slice(0, i) : p;
							const innerPath = i >= 0 ? p.slice(i + 2) : undefined;
							dispatch('open', { source, path: outerPath, kind: e.detail.kind, archivePath: innerPath });
						}}
						on:openDir={(e) => {
							if (e.detail.prefix.startsWith(path + '::')) {
								archivePrefix = e.detail.prefix;
							} else {
								dispatch('navigateDir', e.detail);
							}
						}}
					/>
				{:else if codeLines.length === 0 && metaLines.length === 0 && duplicatePaths.length === 0}
					<div class="no-content">No text content or metadata available for this file.</div>
				{:else}
					<CodeViewer
						{codeLines}
						{lineOffsets}
						{selection}
						{wordWrap}
						on:lineselect={(e) => {
							selection = e.detail.selection;
							dispatch('lineselect', e.detail);
						}}
					/>
				{/if}
				{#if pagedMode && !noMoreForward}
					<div class="load-sentinel">
						{#if loadingForward}
							<span class="sentinel-msg">Loading…</span>
						{/if}
					</div>
				{/if}
			</div>
		{/if}
	{/if}
</div>

<style>
	.file-viewer {
		display: flex;
		flex-direction: column;
		height: 100%;
		overflow: hidden;
	}

	.status {
		padding: 24px;
		color: var(--text-muted);
		text-align: center;
	}

	.status.error {
		color: #f85149;
	}

	.inline-link {
		background: none;
		border: none;
		padding: 0;
		font: inherit;
		color: var(--accent, #58a6ff);
		cursor: pointer;
		text-decoration: underline;
	}

	.code-container {
		flex: 1;
		overflow: auto;
		background: var(--bg);
	}

	.toolbar {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 12px;
		border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.1));
		background: var(--bg-secondary, rgba(0, 0, 0, 0.2));
	}

	.metadata {
		display: flex;
		gap: 16px;
		margin-left: auto;
		font-size: 12px;
		color: var(--text-muted);
	}

	.meta-item {
		display: flex;
		align-items: center;
	}

	.kind-badge {
		text-transform: uppercase;
		font-size: 10px;
		letter-spacing: 0.05em;
		background: var(--bg-hover);
		border: 1px solid var(--border);
		border-radius: 3px;
		padding: 1px 6px;
	}

	.no-content {
		padding: 24px;
		color: var(--text-dim);
		font-size: 13px;
		text-align: center;
	}

	.toolbar-btn {
		padding: 4px 12px;
		font-size: 12px;
		font-family: var(--font-mono);
		background: var(--bg-hover, rgba(255, 255, 255, 0.05));
		border: 1px solid var(--border, rgba(255, 255, 255, 0.15));
		border-radius: 4px;
		color: var(--text);
		cursor: pointer;
		transition: background 0.15s;
	}

	.toolbar-btn:hover {
		background: var(--bg-hover-strong, rgba(255, 255, 255, 0.1));
	}

	.toolbar-btn:active {
		transform: translateY(1px);
	}

	.meta-panel {
		padding: 8px 16px;
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.1));
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
	}

	.meta-row {
		padding: 2px 0;
		line-height: 1.6;
	}

	.duplicate-row {
		display: flex;
		align-items: baseline;
		gap: 6px;
	}

	.duplicate-label {
		flex-shrink: 0;
		color: var(--accent, #58a6ff);
		font-weight: 600;
	}

	.duplicate-link {
		background: none;
		border: none;
		padding: 0;
		font-family: inherit;
		font-size: inherit;
		color: var(--accent, #58a6ff);
		cursor: pointer;
		text-align: left;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.duplicate-link:hover {
		text-decoration: underline;
	}

	.encrypted-notice {
		padding: 24px 16px;
		color: var(--text-muted);
		font-size: 13px;
	}

	.load-sentinel {
		padding: 8px 16px;
		text-align: center;
	}

	.sentinel-msg {
		font-size: 12px;
		color: var(--text-muted);
		font-family: var(--font-mono);
	}

	.sentinel-btn {
		background: none;
		border: 1px solid var(--border, rgba(255, 255, 255, 0.15));
		border-radius: 4px;
		padding: 4px 12px;
		font-size: 12px;
		font-family: var(--font-mono);
		color: var(--text-muted);
		cursor: pointer;
	}

	.sentinel-btn:hover {
		color: var(--text);
		background: var(--bg-hover);
	}
</style>

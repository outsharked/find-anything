<script lang="ts">
	import { createEventDispatcher, onMount, tick } from 'svelte';
	import { getFile } from '$lib/api';
	import { highlightFile } from '$lib/highlight';
	import DirListing from './DirListing.svelte';
	import ImageViewer from './ImageViewer.svelte';
	import MarkdownViewer from './MarkdownViewer.svelte';
	import CodeViewer from './CodeViewer.svelte';
	import {
		type LineSelection,
		firstLine,
	} from '$lib/lineSelection';
	import { profile } from '$lib/profile';
	import { parseImageDimensions } from '$lib/imageMeta';
	import { marked } from 'marked';

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
	}>();

	let loading = true;
	let error: string | null = null;
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
			showOriginal = fileKind === 'image' || (fileKind === 'pdf' && !isEncrypted && preferOriginal);
		}
	}
	// For images: false = split view (image + metadata side-by-side), true = full-width image
	let imageFullWidth = false;
	// PDF load state — reset whenever the source URL changes
	let pdfLoaded = false;
	$: { rawInlineUrl; pdfLoaded = false; }

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
	// Both images and PDFs can be shown inline, including archive members.
	// The server extracts archive members from the outer ZIP via composite paths.
	$: canViewInline = fileKind === 'image' || (fileKind === 'pdf' && !isEncrypted);
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

	// Render markdown to HTML
	$: renderedMarkdown = markdownFormat && isMarkdown
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

	onMount(async () => {
		try {
			const data = await getFile(source, path, archivePath ?? undefined);

			// Separate line_number=0 entries into: current path (skip), metadata, duplicate paths.
			// Metadata lines always start with '['. Path lines are bare file paths.
			// A path line that doesn't match the current file is a duplicate (dedup alias).
			const compositePath = archivePath ? `${path}::${archivePath}` : path;
			const zeroLines = data.lines.filter((l) => l.line_number === 0);
			const contentLines = data.lines.filter((l) => l.line_number > 0);

			metaLines = [];
			duplicatePaths = [];
			for (const l of zeroLines) {
				if (l.content === compositePath) continue; // current file's own path
				if (l.content.startsWith('[')) {
					metaLines.push({ content: l.content });
				} else {
					duplicatePaths.push(l.content);
				}
			}

			const contents = contentLines.map((l) => l.content);
			lineOffsets = contentLines.map((l) => l.line_number);
			rawContent = contents.join('\n');
			highlightedCode = highlightFile(contents, path);
			mtime = data.mtime;
			size = data.size;
			fileKind = data.file_kind ?? null;
			indexingError = data.indexing_error ?? null;
			isEncrypted = fileKind === 'pdf' && contentLines.length === 1 && contentLines[0].content === 'Content encrypted';
			// Default: images always show original; PDFs show original when opened from tree/dir
			// (preferOriginal=true). Search-result opens pass preferOriginal=false to show
			// extracted text context.
			showOriginal = fileKind === 'image' || (fileKind === 'pdf' && !isEncrypted && preferOriginal);
			imageFullWidth = false;
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}

		const ln = firstLine(selection);
		if (ln !== null) {
			await tick();
			scrollToLine(ln);
		}
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
</script>

<div class="file-viewer">
	{#if loading}
		<div class="status">Loading…</div>
	{:else if error}
		<div class="status error">{error}</div>
	{:else}
		{#if indexingError}
			<div class="indexing-error-banner">
				⚠ Indexing error: <span class="error-text">{indexingError}</span>
			</div>
		{/if}
		<div class="toolbar">
			<button class="toolbar-btn" on:click={toggleWordWrap} title="Toggle word wrap">
				{wordWrap ? '⊟' : '⊞'} Wrap
			</button>
			{#if isMarkdown}
				<button class="toolbar-btn" on:click={toggleMarkdownFormat} title="Toggle markdown formatting">
					📝 {markdownFormat ? 'Raw' : 'Format'}
				</button>
			{/if}
			{#if canViewInline}
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
					{isArchiveMember || fileKind === 'archive' ? 'Download Archive' : 'Download Original'}
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
			{:else}
				<!-- PDF / other inline kind -->
				<div class="original-panel">
					{#if !pdfLoaded}<div class="pdf-loading"><div class="pdf-spinner"></div></div>{/if}
					<iframe src={rawInlineUrl} title="Original file" class="original-iframe"
						class:iframe-hidden={!pdfLoaded}
						on:load={() => pdfLoaded = true}></iframe>
				</div>
			{/if}
		{:else}
			<!-- Extracted text / code view -->
			<div class="code-container">
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
				{#if markdownFormat && isMarkdown}
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

	.original-panel {
		flex: 1;
		overflow: auto;
		display: flex;
		flex-direction: column;
		background: var(--bg);
	}

	.original-iframe {
		flex: 1;
		width: 100%;
		height: 100%;
		border: none;
		min-height: 400px;
	}

	.iframe-hidden {
		display: none;
	}

	.pdf-loading {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.pdf-spinner {
		width: 32px;
		height: 32px;
		border: 3px solid rgba(255, 255, 255, 0.08);
		border-top-color: var(--accent, #58a6ff);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		to { transform: rotate(360deg); }
	}

	.indexing-error-banner {
		padding: 8px 16px;
		background: rgba(230, 162, 60, 0.12);
		border-bottom: 1px solid rgba(230, 162, 60, 0.3);
		color: #e6a23c;
		font-size: 12px;
		display: flex;
		align-items: baseline;
		gap: 6px;
		flex-shrink: 0;
	}

	.encrypted-notice {
		padding: 24px 16px;
		color: var(--text-muted);
		font-size: 13px;
	}

	.error-text {
		color: var(--text-muted);
		font-family: var(--font-mono);
		word-break: break-all;
	}
</style>

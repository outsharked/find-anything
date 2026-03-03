<script lang="ts">
	import { createEventDispatcher, onMount, tick } from 'svelte';
	import { getFile } from '$lib/api';
	import { highlightFile } from '$lib/highlight';
	import {
		type LineSelection,
		selectionSet,
		firstLine,
		toggleLine
	} from '$lib/lineSelection';
	import { profile } from '$lib/profile';
	import { marked } from 'marked';

	export let source: string;
	export let path: string;
	export let archivePath: string | null = null;
	export let selection: LineSelection = [];

	const dispatch = createEventDispatcher<{
		lineselect: { selection: LineSelection };
		open: { source: string; path: string; kind: string; archivePath?: string };
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
	let indexingError: string | null = null;
	/** Metadata lines (line_number === 0, excluding the path line itself). */
	let metaLines: { content: string }[] = [];
	/** Paths of duplicate/canonical copies of this file (dedup aliases). */
	let duplicatePaths: string[] = [];

	// Original file view
	let showOriginal = false;
	// For images: false = split view (image + metadata side-by-side), true = full-width image
	let imageFullWidth = false;
	// Image load state — reset whenever the source URL changes
	let imageLoaded = false;
	let imageError = false;
	$: {
		rawInlineUrl;
		imageLoaded = false;
		imageError = false;
	}
	// Parse image dimensions from metadata lines for aspect-ratio placeholder.
	// Handles both EXIF ([EXIF:ImageWidth]/[EXIF:ImageLength]) and basic extractor
	// ([IMAGE:dimensions] WxH) formats.
	let imgWidth: number | null = null;
	let imgHeight: number | null = null;
	$: {
		imgWidth = null;
		imgHeight = null;
		for (const l of metaLines) {
			const w = l.content.match(/^\[EXIF:ImageWidth\]\s+(\d+)/);
			if (w) imgWidth = parseInt(w[1]);
			const h = l.content.match(/^\[EXIF:ImageLength\]\s+(\d+)/);
			if (h) imgHeight = parseInt(h[1]);
			const dims = l.content.match(/^\[IMAGE:dimensions\]\s+(\d+)x(\d+)/);
			if (dims) { imgWidth = parseInt(dims[1]); imgHeight = parseInt(dims[2]); }
		}
	}
	$: placeholderStyle = (imgWidth && imgHeight)
		? `width: min(${imgWidth}px, 100%); aspect-ratio: ${imgWidth} / ${imgHeight}; max-height: min(${imgHeight}px, 100%); min-height: 0;`
		: '';
	// archivePath is set when this file is a member of an archive.
	// path is always the outer (real) file path — it never contains '::'.
	$: isArchiveMember = archivePath !== null;
	// Download/stream URL for the outer file (used for download link and PDF iframe).
	$: rawUrl = `/api/v1/raw?source=${encodeURIComponent(source)}&path=${encodeURIComponent(path)}`;
	// For inline image display, use the composite path for archive members so the
	// server extracts the member from the outer ZIP.
	$: rawInlinePath = archivePath ? `${path}::${archivePath}` : path;
	// Both images and PDFs can be shown inline, including archive members.
	// The server extracts archive members from the outer ZIP via composite paths.
	$: canViewInline = fileKind === 'image' || fileKind === 'pdf';
	// For images the browser can't render natively, request server-side PNG conversion.
	// Check the member's own extension for archive members.
	const BROWSER_IMAGE_EXTS = new Set(['jpg','jpeg','png','gif','webp','svg','svgz','avif','bmp','ico']);
	$: imageExtPath = archivePath ?? path;
	$: needsConversion = fileKind === 'image' && !BROWSER_IMAGE_EXTS.has((imageExtPath.split('.').pop() ?? '').toLowerCase());
	$: rawInlineUrl = needsConversion
		? `/api/v1/raw?source=${encodeURIComponent(source)}&path=${encodeURIComponent(rawInlinePath)}&convert=png`
		: `/api/v1/raw?source=${encodeURIComponent(source)}&path=${encodeURIComponent(rawInlinePath)}`;
	$: fileName = path.split('/').pop() ?? path;

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
			// Default to showing the original for images; extracted text for everything else.
			showOriginal = fileKind === 'image';
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

	function handleLineClick(lineNum: number, e: MouseEvent) {
		let next: LineSelection;
		if (e.ctrlKey || e.metaKey) {
			next = toggleLine(selection, lineNum);
		} else if (e.shiftKey && selection.length > 0) {
			const anchor = firstLine(selection)!;
			next = [anchor <= lineNum ? [anchor, lineNum] : [lineNum, anchor]];
		} else {
			next = [lineNum];
		}
		selection = next;
		dispatch('lineselect', { selection: next });
	}

	$: codeLines = highlightedCode ? highlightedCode.split('\n') : [];
	$: highlightedSet = selectionSet(selection);
	$: arrowLine = firstLine(selection);
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
			<a href={rawUrl} download={fileName} class="toolbar-btn">
				{isArchiveMember ? 'Download Archive' : 'Download Original'}
			</a>
			<div class="metadata">
				{#if fileKind}
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
				{#if imageFullWidth}
					<!-- Full-width scrollable image -->
					<div class="image-full-panel">
						{#if imageError}
							<div class="img-placeholder img-placeholder--error" style={placeholderStyle}>Image unavailable</div>
						{:else}
							{#if !imageLoaded}<div class="img-placeholder img-placeholder--loading" style={placeholderStyle}></div>{/if}
							<img src={rawInlineUrl} alt={path}
								class="image-full" class:img-hidden={!imageLoaded}
								on:load={() => imageLoaded = true}
								on:error={() => imageError = true} />
						{/if}
					</div>
				{:else}
					<!-- Split view: image left, metadata right -->
					<div class="image-split-panel">
						<div class="image-split-left">
							{#if imageError}
								<div class="img-placeholder img-placeholder--error" style={placeholderStyle}>Image unavailable</div>
							{:else}
								{#if !imageLoaded}<div class="img-placeholder img-placeholder--loading" style={placeholderStyle}></div>{/if}
								<img src={rawInlineUrl} alt={path}
									class="image-split-img" class:img-hidden={!imageLoaded}
									on:load={() => imageLoaded = true}
									on:error={() => imageError = true} />
							{/if}
						</div>
						<div class="image-split-right">
							{#if metaLines.length > 0 || duplicatePaths.length > 0}
								{#each duplicatePaths as dup}
									<div class="meta-row duplicate-row">
										<span class="duplicate-label">DUPLICATE:</span>
										<button class="duplicate-link" on:click={() => openDuplicate(dup)}>{dup}</button>
									</div>
								{/each}
								{#each metaLines as meta}
									<div class="meta-row">{meta.content}</div>
								{/each}
							{:else}
								<div class="no-content">No metadata available.</div>
							{/if}
						</div>
					</div>
				{/if}
			{:else}
				<!-- PDF / other inline kind -->
				<div class="original-panel">
					<iframe src={rawInlineUrl} title="Original file" class="original-iframe"></iframe>
				</div>
			{/if}
		{:else}
			<!-- Extracted text / code view -->
			<div class="code-container">
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
					<div class="markdown-content">
						{@html renderedMarkdown}
					</div>
				{:else if codeLines.length === 0 && metaLines.length === 0 && duplicatePaths.length === 0}
					<div class="no-content">No text content or metadata available for this file.</div>
				{:else}
					<table class="code-table" cellspacing="0" cellpadding="0">
						<tbody>
							{#each codeLines as line, i}
								{@const lineNum = lineOffsets[i] ?? i + 1}
								<!-- svelte-ignore a11y-click-events-have-key-events -->
								<!-- svelte-ignore a11y-no-static-element-interactions -->
								<tr
									id="line-{lineNum}"
									class="code-row"
									class:target={highlightedSet.has(lineNum)}
									on:click={(e) => handleLineClick(lineNum, e)}
								>
									<td class="td-ln">{lineNum}</td>
									<td class="td-arrow">{lineNum === arrowLine ? '▶' : ''}</td>
									<td class="td-code" class:wrap={wordWrap}><code>{@html line}</code></td>
								</tr>
							{/each}
						</tbody>
					</table>
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

	.code-table {
		width: 100%;
		border-collapse: collapse;
		font-family: var(--font-mono);
		font-size: 13px;
		line-height: 1.6;
	}

	.code-row {
		border-left: 2px solid transparent;
		cursor: pointer;
	}

	.code-row:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.04));
	}

	.code-row.target {
		background: var(--match-line-bg);
		border-left-color: var(--match-border);
	}

	.td-ln {
		width: 1%;
		min-width: 52px;
		white-space: nowrap;
		padding: 0 12px 0 8px;
		text-align: right;
		color: var(--text-dim);
		user-select: none;
		vertical-align: top;
	}

	.td-arrow {
		width: 16px;
		white-space: nowrap;
		color: var(--accent);
		font-size: 10px;
		user-select: none;
		vertical-align: top;
	}

	.td-code {
		width: 100%;
		padding: 0 16px 0 4px;
		white-space: pre;
		vertical-align: top;
	}

	.td-code.wrap {
		white-space: pre-wrap;
		word-break: break-word;
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

	/* Image split view */
	.image-split-panel {
		flex: 1;
		display: flex;
		flex-direction: row;
		overflow: hidden;
		min-height: 0;
	}

	.image-split-left {
		flex: 1;
		overflow: auto;
		display: flex;
		align-items: center;
		justify-content: center;
		border-right: 1px solid var(--border, rgba(255, 255, 255, 0.1));
		padding: 16px;
		min-width: 0;
	}

	.image-split-img {
		max-width: 100%;
		max-height: 100%;
		object-fit: contain;
	}

	.image-split-right {
		width: 300px;
		flex-shrink: 0;
		overflow-y: auto;
		padding: 12px 16px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		background: var(--bg-secondary);
	}

	/* Image full-width view */
	.image-full-panel {
		flex: 1;
		overflow-y: auto;
		background: var(--bg);
	}

	.image-full {
		width: 100%;
		height: auto;
		display: block;
	}

	/* Image placeholder (loading / error) */
	.img-placeholder {
		width: 100%;
		min-height: 200px;
		display: flex;
		align-items: center;
		justify-content: center;
		border-radius: 4px;
		font-size: 12px;
		color: var(--text-dim);
	}

	.img-placeholder--loading {
		background: var(--bg-hover, rgba(255, 255, 255, 0.04));
		animation: img-pulse 1.4s ease-in-out infinite;
	}

	.img-placeholder--error {
		background: var(--bg-hover, rgba(255, 255, 255, 0.04));
		color: var(--text-muted);
	}

	@keyframes img-pulse {
		0%, 100% { opacity: 0.5; }
		50%       { opacity: 1;   }
	}

	.img-hidden {
		display: none;
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

	.error-text {
		color: var(--text-muted);
		font-family: var(--font-mono);
		word-break: break-all;
	}

	/* Markdown rendering styles — child selectors use :global() because the
	   content is injected via {@html} and Svelte can't see those elements. */
	.markdown-content {
		padding: 32px 48px;
		max-width: 900px;
		margin: 0 auto;
		color: var(--text);
		line-height: 1.7;
	}

	.markdown-content :global(h1),
	.markdown-content :global(h2),
	.markdown-content :global(h3) {
		border-bottom: 1px solid var(--border);
		padding-bottom: 0.4em;
		margin-top: 32px;
		margin-bottom: 20px;
		font-weight: 600;
	}

	.markdown-content :global(h1) {
		font-size: 2em;
		margin-top: 0;
	}

	.markdown-content :global(h2) {
		font-size: 1.5em;
	}

	.markdown-content :global(h3) {
		font-size: 1.25em;
	}

	.markdown-content :global(h4) {
		font-size: 1.1em;
		font-weight: 600;
		margin-top: 24px;
		margin-bottom: 16px;
	}

	.markdown-content :global(h5),
	.markdown-content :global(h6) {
		font-size: 1em;
		font-weight: 600;
		margin-top: 20px;
		margin-bottom: 12px;
	}

	.markdown-content :global(a) {
		color: var(--accent);
		text-decoration: none;
	}

	.markdown-content :global(a:hover) {
		text-decoration: underline;
	}

	.markdown-content :global(code) {
		background: var(--bg-secondary);
		padding: 0.2em 0.4em;
		border-radius: 3px;
		font-family: var(--font-mono);
		font-size: 0.9em;
	}

	.markdown-content :global(pre) {
		background: var(--bg-secondary);
		padding: 16px;
		border-radius: 6px;
		overflow-x: auto;
		margin: 20px 0;
		line-height: 1.5;
	}

	.markdown-content :global(pre code) {
		background: none;
		padding: 0;
	}

	.markdown-content :global(blockquote) {
		border-left: 4px solid var(--accent);
		padding: 8px 0 8px 20px;
		margin: 24px 0;
		color: var(--text-muted);
	}

	.markdown-content :global(table) {
		border-collapse: collapse;
		width: 100%;
		margin: 24px 0;
	}

	.markdown-content :global(th),
	.markdown-content :global(td) {
		border: 1px solid var(--border);
		padding: 8px 12px;
		text-align: left;
	}

	.markdown-content :global(th) {
		background: var(--bg-secondary);
		font-weight: 600;
	}

	.markdown-content :global(tr:nth-child(even)) {
		background: var(--bg-hover);
	}

	.markdown-content :global(img) {
		max-width: 100%;
		height: auto;
	}

	.markdown-content :global(ul),
	.markdown-content :global(ol) {
		padding-left: 2em;
		margin: 16px 0;
	}

	.markdown-content :global(li) {
		margin: 6px 0;
		line-height: 1.6;
	}

	.markdown-content :global(li > p) {
		margin: 4px 0;
	}

	.markdown-content :global(p) {
		margin: 16px 0;
	}

	.markdown-content :global(p:first-child) {
		margin-top: 0;
	}

	.markdown-content :global(hr) {
		border: none;
		border-top: 1px solid var(--border);
		margin: 32px 0;
	}
</style>

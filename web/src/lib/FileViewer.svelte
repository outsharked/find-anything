<script lang="ts">
	import { createEventDispatcher, onMount, afterUpdate, onDestroy, tick } from 'svelte';
	import IconDownload from '$lib/icons/IconDownload.svelte';
	import IconCopy from '$lib/icons/IconCopy.svelte';
	import IconCheck from '$lib/icons/IconCheck.svelte';
	import IconFolder from '$lib/icons/IconFolder.svelte';
	import IconShareApple from '$lib/icons/IconShareApple.svelte';
	import IconShareWindows from '$lib/icons/IconShareWindows.svelte';
	import IconShareAndroid from '$lib/icons/IconShareAndroid.svelte';
	import IconEmail from '$lib/icons/IconEmail.svelte';
	import IconWrapOn from '$lib/icons/IconWrapOn.svelte';
	import IconWrapOff from '$lib/icons/IconWrapOff.svelte';
	import { getFile, createLink } from '$lib/api';
	import { fileViewPageSize, contentLineStart, tabWidth as serverTabWidth } from '$lib/settingsStore';
	import { highlightFile } from '$lib/highlight';
	import DirListing from './DirListing.svelte';
	import AudioViewer from './AudioViewer.svelte';
	import DirectImageViewer from './DirectImageViewer.svelte';
	import MetaDrawer from './MetaDrawer.svelte';
	import MarkdownViewer from './MarkdownViewer.svelte';
	import CodeViewer from './CodeViewer.svelte';
	import PdfViewer from './PdfViewer.svelte';
	import VideoViewer from './VideoViewer.svelte';
	import { parseMetaTags } from '$lib/metaTags';
	import { buildExplorerUrl } from '$lib/explorerUrl';
	import FileStatusBanner from './FileStatusBanner.svelte';
	import {
		type LineSelection,
		firstLine,
	} from '$lib/lineSelection';
	import { profile } from '$lib/profile';
	import { publicUrl } from '$lib/settingsStore';
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
	let duplicatesModalOpen = false;

	// Original file view
	let showOriginal = false;
	// Track previous preferOriginal to detect changes after the component is mounted
	// (e.g. same file re-opened from a different entry point without remounting).
	let _prevPreferOriginal = preferOriginal;
	$: if (preferOriginal !== _prevPreferOriginal) {
		_prevPreferOriginal = preferOriginal;
		if (fileKind !== null) {
			showOriginal = fileKind === 'image' || fileKind === 'video' || fileKind === 'audio' || fileKind === 'dicom' || (fileKind === 'pdf' && !isEncrypted && preferOriginal) || isSvg;
		}
	}
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
	// For archive members, the raw endpoint only supports ZIP archives — RAR/TAR/7z members
	// cannot be extracted for inline viewing.  All archives in the composite path (outer +
	// any intermediate) must be ZIPs for the server to serve the member.
	$: canServeArchiveMember = !isArchiveMember || (
		outerExt === 'zip' &&
		(archivePath ?? '').split('::').slice(0, -1).every(
			part => (part.split('.').pop() ?? '').toLowerCase() === 'zip'
		)
	);
	// Images, PDFs, videos, and audio can be shown inline when the file is directly accessible
	// or is a member of a ZIP archive.
	$: isSvg = /\.svgz?$/i.test(archivePath ?? path);
	$: canViewInline = (fileKind === 'dicom' && !isArchiveMember) || (canServeArchiveMember && (fileKind === 'image' || (fileKind === 'pdf' && !isEncrypted) || fileKind === 'video' || fileKind === 'audio' || isSvg));
	// Unified image/dicom view URL — server determines the representation.
	$: viewUrl = `/api/v1/view?source=${encodeURIComponent(source)}&path=${encodeURIComponent(rawInlinePath)}`;
	// Raw URL for audio/video/PDF/SVG streaming (range requests required for media).
	$: rawInlineUrl = `/api/v1/raw?source=${encodeURIComponent(source)}&path=${encodeURIComponent(rawInlinePath)}`;
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

	function triggerDownload(url: string, filename: string) {
		const a = document.createElement('a');
		a.href = url;
		a.download = filename;
		document.body.appendChild(a);
		a.click();
		document.body.removeChild(a);
	}

	// Detect if file is markdown
	$: isMarkdown = path.endsWith('.md') || path.endsWith('.markdown');

	// Detect if file is RTF (check member extension for archive members)
	$: isRtf = (archivePath ?? path).toLowerCase().endsWith('.rtf');

	// Word wrap preference (default: false for code, true for text files)
	$: wordWrap = $profile.wordWrap ?? false;

	let hasOverflow = false;
	let overflowObserver: ResizeObserver | null = null;

	function checkOverflow() {
		if (!codeContainer) return;
		hasOverflow = codeContainer.scrollWidth > codeContainer.clientWidth;
	}

	afterUpdate(() => {
		if (!codeContainer) return;
		checkOverflow();
		if (!overflowObserver) {
			overflowObserver = new ResizeObserver(checkOverflow);
			overflowObserver.observe(codeContainer);
		}
	});

	// Tab width: user profile overrides server default.
	$: tabWidth = $profile.tabWidth ?? $serverTabWidth;

	// Markdown format preference
	$: markdownFormat = $profile.markdownFormat ?? false;

	// RTF format preference
	$: rtfFormat = $profile.rtfFormat ?? false;

	// RTF rendered HTML — rendered client-side via rtf.js (dynamically imported).
	let renderedRtf = '';
	let rtfFetchedForPath = '';
	let rtfError = false;

	$: if (rtfFormat && isRtf && rtfFetchedForPath !== rawInlinePath) {
		fetchRtfHtml(rawInlinePath);
	}

	async function fetchRtfHtml(forPath: string) {
		rtfFetchedForPath = forPath;
		renderedRtf = '';
		rtfError = false;
		try {
			const url = `/api/v1/raw?source=${encodeURIComponent(source)}&path=${encodeURIComponent(forPath)}`;
			const resp = await fetch(url);
			if (!resp.ok) { rtfError = true; return; }
			const arrayBuffer = await resp.arrayBuffer();

			// Dynamic import — only fetched the first time an RTF file is formatted.
			// eslint-disable-next-line @typescript-eslint/no-explicit-any
			const { RTFJS } = await import('rtf.js') as any;
			const doc = new RTFJS.Document(arrayBuffer);
			const elements = await doc.render();

			const container = document.createElement('div');
			for (const el of elements) container.appendChild(el);
			const html = container.innerHTML;
			if (html.trim()) renderedRtf = html;
			else rtfError = true;
		} catch {
			rtfError = true;
		}
	}

	function toggleRtfFormat() {
		$profile.rtfFormat = !rtfFormat;
	}

	// "Open in Explorer" — visible when a source root is configured for this source.
	// For archive members we use the outer file path (Explorer can select the archive
	// but cannot navigate into a virtual member path).
	$: canOpenInExplorer = !!($profile.sourceRoots?.[source]?.trim()) && !!$profile.handlerInstalled;

	let explorerLaunching = false;

	// ── Share ────────────────────────────────────────────────────────────────────
	// Detect client OS for icon selection (evaluated once in the browser).
	const _ua = typeof navigator !== 'undefined' ? navigator.userAgent : '';
	const shareIconOs: 'apple' | 'windows' | 'android' =
		/iPhone|iPad|iPod|Macintosh/i.test(_ua) ? 'apple' :
		/Windows/i.test(_ua) ? 'windows' : 'android';

	let shareDialogOpen = false;
	let shareUrl = '';
	let shareError: string | null = null;
	let shareLinkBusy = false;
	let shareLinkCopied = false;
	// 86400 = 1 day, 604800 = 1 week, 2592000 = 30 days, 0 = never
	let shareTtl: number = 604800;

	function openShareDialog() {
		shareUrl = '';
		shareError = null;
		shareLinkCopied = false;
		shareDialogOpen = true;
	}

	async function createShareLink() {
		if (shareLinkBusy) return;
		shareLinkBusy = true;
		shareError = null;
		try {
			const resp = await createLink(source, path, archivePath, shareTtl);
			const origin = $publicUrl ?? window.location.origin;
			const url = origin + resp.url;
			shareUrl = url;
			// On platforms with native share, hand off immediately.
			if (navigator.share) {
				shareDialogOpen = false;
				await navigator.share({ url, title: fileName });
			}
		} catch (err) {
			if (!(err instanceof DOMException && err.name === 'AbortError')) {
				shareError = 'Failed to generate share link.';
			}
		} finally {
			shareLinkBusy = false;
		}
	}

	function fallbackCopy(text: string, done: () => void) {
		const ta = document.createElement('textarea');
		ta.value = text;
		ta.style.cssText = 'position:fixed;opacity:0';
		document.body.appendChild(ta);
		ta.focus();
		ta.select();
		try { document.execCommand('copy'); done(); } finally { document.body.removeChild(ta); }
	}

	function copyShareLink() {
		const url = shareUrl;
		const done = () => { shareLinkCopied = true; setTimeout(() => (shareLinkCopied = false), 2000); };
		if (navigator.clipboard) {
			navigator.clipboard.writeText(url).then(done).catch(() => fallbackCopy(url, done));
		} else {
			fallbackCopy(url, done);
		}
	}

	function openInExplorer() {
		const root = ($profile.sourceRoots ?? {})[source] ?? '';
		if (!root) return;
		const a = document.createElement('a');
		a.href = buildExplorerUrl(root, path);
		document.body.appendChild(a);
		a.click();
		document.body.removeChild(a);
		explorerLaunching = true;
		setTimeout(() => (explorerLaunching = false), 3000);
	}

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

	/** Adjust raw line_offsets from server to display line numbers. */
	function adjustOffsets(raw: number[]): number[] {
		const adj = $contentLineStart - 1;
		return adj > 0 ? raw.map(n => n - adj) : raw;
	}

	/** Rebuild rawContent / highlightedCode / lineOffsets from accumulated lines. */
	async function updateCodeState() {
		lineOffsets = allLineOffsets;
		rawContent = allContentLines.join('\n');
		highlightedCode = await highlightFile(allContentLines, path);
	}

	async function applyFileData(data: import('$lib/api').FileResponse, isInitial: boolean) {
		contentUnavailable = data.content_unavailable ?? false;
		if (contentUnavailable) return;
		error = null;

		// Separate metadata entries from duplicate paths.
		// Entries tagged [fa:duplicate] are duplicate paths; all others are metadata content.
		// Actual duplicate paths also come from the dedicated duplicate_paths field (schema v3).
		const compositePath = archivePath ? `${path}::${archivePath}` : path;
		metaLines = [];
		duplicatePaths = [];
		for (const s of data.metadata) {
			if (!s || s === compositePath) continue;
			if (s.startsWith('[fa:duplicate] ')) {
				duplicatePaths.push(s.slice('[fa:duplicate] '.length));
			} else {
				metaLines.push({ content: s });
			}
		}
		for (const dup of (data.duplicate_paths ?? [])) {
			if (dup && !duplicatePaths.includes(dup)) duplicatePaths.push(dup);
		}

		lineOffsets = data.line_offsets && data.line_offsets.length > 0
			? adjustOffsets(data.line_offsets)
			: data.lines.map((_, i) => i + 1);
		rawContent = data.lines.join('\n');
		highlightedCode = await highlightFile(data.lines, path);
		mtime = data.mtime;
		size = data.size;
		fileKind = data.file_kind ?? null;
		indexingError = data.indexing_error ?? null;
		isEncrypted = fileKind === 'pdf' && data.lines.length === 1 && data.lines[0] === 'Content encrypted';
		// For kinds with no viewer toggle (image, audio, dicom), always sync showOriginal so
		// live-update reloads (isInitial=false) work correctly after an upgrade scan changes the kind.
		// For PDF/video/SVG, only set on initial load to preserve the user's toggle preference.
		const noToggleKind = fileKind === 'image' || fileKind === 'audio' || fileKind === 'dicom';
		if (isInitial || noToggleKind) {
			showOriginal = fileKind === 'image' || fileKind === 'video' || fileKind === 'audio' || fileKind === 'dicom' || (fileKind === 'pdf' && !isEncrypted && preferOriginal) || isSvg;
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
			if (!s || s === compositePath) continue;
			if (s.startsWith('[fa:duplicate] ')) {
				duplicatePaths.push(s.slice('[fa:duplicate] '.length));
			} else {
				metaLines.push({ content: s });
			}
		}
		for (const dup of (data.duplicate_paths ?? [])) {
			if (dup && !duplicatePaths.includes(dup)) duplicatePaths.push(dup);
		}
		if (isInitial) {
			isEncrypted = fileKind === 'pdf' && data.lines.length === 1 && data.lines[0] === 'Content encrypted';
		}
		const noToggleKind = fileKind === 'image' || fileKind === 'audio' || fileKind === 'dicom';
		if (isInitial || noToggleKind) {
			showOriginal = fileKind === 'image' || fileKind === 'video' || fileKind === 'audio' || fileKind === 'dicom' || (fileKind === 'pdf' && !isEncrypted && preferOriginal) || isSvg;
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
					? adjustOffsets(data.line_offsets)
					: data.lines.map((_, i) => anchorOffset + i + 1);
				allContentLines = [...data.lines];
				allLineOffsets = pageOffsets;
				totalLines = data.total_lines;
				forwardOffset = anchorOffset + data.lines.length;
				backwardOffset = anchorOffset;
				noMoreForward = forwardOffset >= totalLines;
				noMoreBackward = anchorOffset === 0;
				await updateCodeState();
			} else {
				// Single-page (full file) mode — identical to previous behaviour.
				await applyFileData(data, isInitial);
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
				? adjustOffsets(data.line_offsets)
				: data.lines.map((_, i) => forwardOffset + i + 1);
			allContentLines = [...allContentLines, ...data.lines];
			allLineOffsets = [...allLineOffsets, ...pageOffsets];
			forwardOffset += data.lines.length;
			noMoreForward = forwardOffset >= totalLines;
			await updateCodeState();
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
				? adjustOffsets(data.line_offsets)
				: data.lines.map((_, i) => prevOffset + i + 1);

			// Preserve scroll position when prepending.
			const oldScrollHeight = codeContainer.scrollHeight;
			const oldScrollTop = codeContainer.scrollTop;

			allContentLines = [...data.lines, ...allContentLines];
			allLineOffsets = [...pageOffsets, ...allLineOffsets];
			backwardOffset = prevOffset;
			noMoreBackward = prevOffset === 0;
			await updateCodeState();

			await tick();
			codeContainer.scrollTop = oldScrollTop + (codeContainer.scrollHeight - oldScrollHeight);
		} catch { /* silent */ }
		loadingBackward = false;
	}

	onMount(async () => {
		await loadFile(true);
	});

	onDestroy(() => {
		overflowObserver?.disconnect();
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
			{#if canViewInline && (fileKind === 'pdf' || fileKind === 'video')}
				<button class="toolbar-btn" on:click={() => showOriginal = !showOriginal}>
					{showOriginal ? 'View Extracted' : 'View Original'}
				</button>
			{:else if isSvg && canViewInline}
				<button class="toolbar-btn" on:click={() => showOriginal = !showOriginal}>
					{showOriginal ? 'View Source' : 'View SVG'}
				</button>
			{/if}
			{#if !(showOriginal && canViewInline) && fileKind !== 'image' && fileKind !== 'video' && fileKind !== 'audio' && fileKind !== 'dicom' && (hasOverflow || wordWrap)}
				<button class="toolbar-btn toolbar-icon-btn" on:click={toggleWordWrap} title={wordWrap ? 'Disable word wrap' : 'Enable word wrap'}>
					{#if wordWrap}
						<IconWrapOn />
					{:else}
						<IconWrapOff />
					{/if}
				</button>
			{/if}
			{#if isMarkdown && !markdownTooLarge}
				<button class="toolbar-btn" on:click={toggleMarkdownFormat} title="Toggle markdown formatting">
					{markdownFormat ? 'Plain' : 'Formatted'}
				</button>
			{/if}
			{#if isRtf}
				<button class="toolbar-btn" on:click={toggleRtfFormat} title="Toggle RTF formatting">
					{rtfFormat ? 'Plain' : 'Formatted'}
				</button>
			{/if}
			{#if canOpenInExplorer}
				<button class="toolbar-btn explorer-btn download-icon-btn" style={explorerLaunching ? 'cursor: progress' : ''} on:click={openInExplorer} title="Open in Explorer">
					<IconFolder />
				</button>
			{/if}
			{#if canDownloadMember}
				<button class="toolbar-btn download-icon-btn" on:click={() => triggerDownload(rawInlineUrl, memberFileName)} title="Download">
					<IconDownload />
				</button>
				<button class="toolbar-btn download-archive-btn" on:click={() => triggerDownload(rawUrl, fileName)} title="Download Archive">
					<IconDownload />
					archive
				</button>
			{:else}
				<button
					class="toolbar-btn {isArchiveMember || fileKind === 'archive' ? 'download-archive-btn' : 'download-icon-btn'}"
					on:click={() => triggerDownload(rawUrl, fileName)}
					title={isArchiveMember || fileKind === 'archive' ? 'Download Archive' : 'Download'}>
					<IconDownload />
					{#if isArchiveMember || fileKind === 'archive'} archive{/if}
				</button>
			{/if}
			<button class="toolbar-btn share-icon-btn" on:click={openShareDialog} title="Share">
				{#if shareIconOs === 'apple'}
					<IconShareApple />
				{:else if shareIconOs === 'windows'}
					<IconShareWindows />
				{:else}
					<IconShareAndroid />
				{/if}
			</button>
			<div class="metadata">
				{#if duplicatePaths.length > 0}
					<button class="meta-item dup-badge" on:click={() => duplicatesModalOpen = true}>
						{duplicatePaths.length} {duplicatePaths.length === 1 ? 'duplicate' : 'duplicates'}
					</button>
				{/if}
				{#if fileKind && fileKind !== 'raw'}
					<span class="meta-item kind-badge">{fileKind}</span>
				{/if}
				{#if size !== null}
					<span class="meta-item">{formatSize(size)}</span>
				{/if}
				{#if mtime !== null}
					<span class="meta-item">{formatDate(mtime)}</span>
				{/if}
			</div>
		</div>

		{#if duplicatesModalOpen}
			<button class="dup-modal-backdrop" on:click={() => duplicatesModalOpen = false} aria-label="Close duplicates"></button>
			<div class="dup-modal" role="dialog" aria-label="Duplicates">
				<div class="dup-modal-header">
					<span class="dup-modal-title">{duplicatePaths.length} {duplicatePaths.length === 1 ? 'Duplicate' : 'Duplicates'}</span>
					<button class="dup-modal-close" on:click={() => duplicatesModalOpen = false}>✕</button>
				</div>
				<div class="dup-modal-list">
					{#each duplicatePaths as dup}
						<button class="dup-modal-link" on:click={() => { duplicatesModalOpen = false; openDuplicate(dup); }}>{dup}</button>
					{/each}
				</div>
			</div>
		{/if}
		{#if showOriginal && canViewInline}
			{#if isSvg}
				<div class="image-viewer-panel">
					<DirectImageViewer src={rawInlineUrl} svgMode={true} />
				</div>
			{:else if fileKind === 'image'}
				<div class="image-viewer-panel">
					<DirectImageViewer src={viewUrl} />
					<MetaDrawer initialOpen={false}>
						{#if metaLines.length > 0}
							{#each metaLines as meta}
								{#each parseMetaTags(meta.content) as tag}
									<div class="meta-row">
										<span class="tag-label">[{tag.label}]</span>
										<span class="tag-value">{tag.value}</span>
									</div>
								{/each}
							{/each}
						{:else}
							<div class="no-meta">No metadata available.</div>
						{/if}
					</MetaDrawer>
				</div>
			{:else if fileKind === 'dicom'}
				<div class="image-viewer-panel">
					<DirectImageViewer src={viewUrl} />
					<MetaDrawer initialOpen={false}>
						{#if metaLines.length > 0}
							{#each metaLines as meta}
								{#each parseMetaTags(meta.content) as tag}
									<div class="meta-row">
										<span class="tag-label">[{tag.label}]</span>
										<span class="tag-value">{tag.value}</span>
									</div>
								{/each}
							{/each}
						{:else}
							<div class="no-meta">No metadata available.</div>
						{/if}
					</MetaDrawer>
				</div>
			{:else if fileKind === 'audio'}
				<AudioViewer
					src={rawInlineUrl}
					{metaLines}
				/>
			{:else if fileKind === 'video'}
				<VideoViewer
					src={rawInlineUrl}
					{metaLines}
				/>
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
				{#if metaLines.length > 0}
					<div class="meta-panel">
						{#each metaLines as meta}
							{#each parseMetaTags(meta.content) as tag}
								<div class="meta-row">
									<span class="tag-label">[{tag.label}]</span>
									<span class="tag-value">{tag.value}</span>
								</div>
							{/each}
						{/each}
					</div>
				{/if}
				{#if markdownTooLarge && markdownFormat}
					<div class="no-content">File too large to render as markdown ({Math.round(rawContent.length / 1024)} KB &gt; {$maxMarkdownRenderKb} KB limit). Showing plain text.</div>
				{/if}
				{#if rtfFormat && isRtf}
					{#if renderedRtf}
						<MarkdownViewer rendered={renderedRtf} />
					{:else if rtfError}
						<div class="no-content">RTF rendering failed.</div>
					{:else}
						<div class="no-content">Converting…</div>
					{/if}
				{:else if markdownFormat && isMarkdown && !markdownTooLarge}
					<MarkdownViewer rendered={String(renderedMarkdown)} />
				{:else if codeLines.length === 0 && metaLines.length === 0 && fileKind === 'archive' && !archivePath}
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
				{:else if codeLines.length === 0 && metaLines.length === 0}
					<div class="no-content">No text content or metadata available for this file.</div>
				{:else}
					<CodeViewer
						{codeLines}
						{lineOffsets}
						{selection}
						{wordWrap}
						{tabWidth}
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

{#if shareDialogOpen}
<!-- svelte-ignore a11y-no-noninteractive-element-interactions -->
<div class="share-overlay" on:click|self={() => shareDialogOpen = false} on:keydown={(e) => e.key === 'Escape' && (shareDialogOpen = false)} role="dialog" aria-modal="true" aria-label="Share">
	<div class="share-dialog">
		<div class="share-dialog-header">
			<span class="share-dialog-title">Share</span>
			<button class="share-close" on:click={() => shareDialogOpen = false} aria-label="Close">✕</button>
		</div>
		<div class="share-dialog-body">
			<p class="share-instructions">Creates a link to this file that anyone with the link can access.</p>
			<div class="share-ttl-row">
				<span class="share-ttl-label">Expires</span>
				<div class="share-ttl-options">
					{#each [{v:86400,l:'1 day'},{v:604800,l:'1 week'},{v:2592000,l:'1 month'},{v:0,l:'Never'}] as opt (opt.v)}
						<label class="share-ttl-opt" class:selected={shareTtl === opt.v}>
							<input type="radio" name="share-ttl" value={opt.v} bind:group={shareTtl} disabled={!!shareUrl} />
							{opt.l}
						</label>
					{/each}
				</div>
			</div>
			{#if shareError}
				<p class="share-msg share-error">{shareError}</p>
			{/if}
			{#if shareUrl}
				<div class="share-link-row">
					<span class="share-link-text">{shareUrl}</span>
					<button class="share-copy-btn" class:copied={shareLinkCopied} on:click={copyShareLink} title="Copy link">
						{#if shareLinkCopied}
							<IconCheck />
						{:else}
							<IconCopy />
						{/if}
					</button>
				</div>
				<div class="share-actions">
					<a class="share-action-btn" href="mailto:?subject={encodeURIComponent('Shared: ' + fileName)}&body={encodeURIComponent(shareUrl)}" rel="noopener">
						<IconEmail />
						Email
					</a>
				</div>
			{:else}
				<div class="share-actions">
					<button class="share-create-btn" on:click={createShareLink} disabled={shareLinkBusy}>
						{shareLinkBusy ? 'Creating…' : 'Create link'}
					</button>
					<button class="share-cancel-btn" on:click={() => shareDialogOpen = false}>Cancel</button>
				</div>
			{/if}
		</div>
	</div>
</div>
{/if}

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

	.dup-badge {
		background: var(--badge-bg);
		border: 1px solid var(--border);
		color: var(--accent);
		border-radius: 3px;
		cursor: pointer;
		font-size: 11px;
		padding: 1px 6px;
		line-height: 1.4;
		font-weight: 600;
	}

	.dup-badge:hover {
		border-color: var(--accent);
	}

	.dup-modal-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.5);
		z-index: 500;
		border: none;
		padding: 0;
		cursor: default;
	}

	.dup-modal {
		position: fixed;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		z-index: 501;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 8px;
		width: min(640px, calc(100vw - 32px));
		max-height: min(400px, calc(100vh - 64px));
		display: flex;
		flex-direction: column;
		box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
	}

	.dup-modal-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 12px 16px;
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
	}

	.dup-modal-title {
		font-weight: 600;
		font-size: 14px;
		color: var(--text);
	}

	.dup-modal-close {
		background: none;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		font-size: 14px;
		padding: 2px 6px;
		border-radius: 3px;
		line-height: 1;
	}

	.dup-modal-close:hover {
		color: var(--text);
		background: var(--bg-hover, rgba(255,255,255,0.08));
	}

	.dup-modal-list {
		overflow-y: auto;
		padding: 8px 0;
		display: flex;
		flex-direction: column;
	}

	.dup-modal-link {
		background: none;
		border: none;
		padding: 7px 16px 7px 32px;
		text-align: left;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--accent);
		cursor: pointer;
		white-space: normal;
		word-break: break-all;
		line-height: 1.5;
		position: relative;
	}

	.dup-modal-link::before {
		content: '•';
		position: absolute;
		left: 16px;
		color: var(--text-muted);
	}

	.dup-modal-link:hover {
		background: var(--bg-hover, rgba(255,255,255,0.06));
		text-decoration: underline;
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

	.toolbar-icon-btn {
		padding: 4px 7px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
	}

	.download-icon-btn,
	.download-archive-btn,
	.share-icon-btn {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		padding: 4px 8px;
	}

	.share-overlay {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.5);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 500;
	}

	.share-dialog {
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 8px;
		min-width: 300px;
		max-width: 480px;
		width: 90%;
		box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
	}

	.share-dialog-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 12px 16px;
		border-bottom: 1px solid var(--border);
	}

	.share-dialog-title {
		font-size: 14px;
		font-weight: 600;
		color: var(--text);
	}

	.share-close {
		background: none;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		font-size: 14px;
		padding: 2px 6px;
		border-radius: 3px;
	}

	.share-close:hover {
		color: var(--text);
		background: var(--bg-hover);
	}

	.share-dialog-body {
		padding: 16px;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.share-instructions {
		font-size: 13px;
		color: var(--text-muted);
		margin: 0;
		line-height: 1.5;
	}

	.share-ttl-row {
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.share-ttl-label {
		font-size: 12px;
		color: var(--text-muted);
		flex-shrink: 0;
	}

	.share-ttl-options {
		display: flex;
		gap: 4px;
		flex-wrap: wrap;
	}

	.share-ttl-opt {
		display: inline-flex;
		align-items: center;
		padding: 3px 10px;
		font-size: 12px;
		font-family: var(--font-mono);
		border: 1px solid var(--border);
		border-radius: 4px;
		cursor: pointer;
		color: var(--text-muted);
		background: var(--bg);
		transition: background 0.1s, color 0.1s, border-color 0.1s;
		user-select: none;
	}

	.share-ttl-opt input[type="radio"] {
		display: none;
	}

	.share-ttl-opt:hover:not(.selected) {
		background: var(--bg-hover);
		color: var(--text);
	}

	.share-ttl-opt.selected {
		background: var(--accent, #58a6ff);
		border-color: var(--accent, #58a6ff);
		color: #fff;
	}

	.share-msg {
		font-size: 13px;
		color: var(--text-muted);
		margin: 0;
	}

	.share-error {
		color: #f85149;
	}

	.share-cancel-btn {
		padding: 6px 16px;
		font-size: 13px;
		font-family: var(--font-mono);
		background: transparent;
		border: 1px solid var(--border);
		border-radius: 4px;
		color: var(--text-muted);
		cursor: pointer;
		transition: border-color 0.15s, color 0.15s;
	}

	.share-cancel-btn:hover {
		border-color: var(--text-muted);
		color: var(--text);
	}

	.share-create-btn {
		padding: 6px 16px;
		font-size: 13px;
		font-family: var(--font-mono);
		background: var(--accent, #58a6ff);
		flex-shrink: 0;
		border: none;
		border-radius: 4px;
		color: #fff;
		cursor: pointer;
		transition: opacity 0.15s;
	}

	.share-create-btn:hover {
		opacity: 0.85;
	}

	.share-create-btn:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.share-link-row {
		display: flex;
		align-items: center;
		gap: 8px;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 8px 10px;
	}

	.share-link-text {
		flex: 1;
		min-width: 0;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.share-copy-btn {
		background: none;
		border: none;
		padding: 2px 4px;
		cursor: pointer;
		color: var(--text-dim);
		display: inline-flex;
		align-items: center;
		border-radius: 3px;
		flex-shrink: 0;
		transition: color 0.15s;
	}

	.share-copy-btn:hover {
		color: var(--accent);
	}

	.share-copy-btn.copied {
		color: #3fb950;
	}

	.share-actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
	}

	.share-action-btn {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 6px 14px;
		font-size: 12px;
		font-family: var(--font-mono);
		background: var(--bg-hover, rgba(255, 255, 255, 0.05));
		border: 1px solid var(--border);
		border-radius: 4px;
		color: var(--text);
		cursor: pointer;
		text-decoration: none;
		transition: background 0.15s;
	}

	.share-action-btn:hover {
		background: var(--bg-hover-strong, rgba(255, 255, 255, 0.1));
	}


	.meta-panel {
		padding: 12px 16px;
		font-family: var(--font-mono);
		font-size: 12px;
	}

	.meta-row {
		padding: 2px 0;
		line-height: 1.6;
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}

	.tag-label {
		color: var(--text-dim);
		flex-shrink: 0;
	}

	.tag-value {
		color: var(--text-muted);
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

	.image-viewer-panel {
		flex: 1;
		display: flex;
		flex-direction: row;
		min-height: 0;
		overflow: hidden;
	}

	.meta-row {
		padding: 2px 0;
		line-height: 1.6;
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}

	.tag-label {
		color: var(--text-dim);
		flex-shrink: 0;
	}

	.tag-value {
		color: var(--text-muted);
	}

	.no-meta {
		padding: 24px;
		color: var(--text-dim);
		font-size: 13px;
		text-align: center;
	}

	@media (max-width: 768px) {
		.download-archive-btn { display: none; }
		.explorer-btn { display: none; }
		.toolbar { flex-wrap: wrap; }
		/* On mobile the file viewer scrolls as a whole; inner panels flow naturally */
		.file-viewer {
			overflow-y: auto;
			height: auto;
			min-height: 100%;
		}
		.image-viewer-panel {
			flex-direction: column;
			flex: none;
			overflow: visible;
		}
	}
</style>

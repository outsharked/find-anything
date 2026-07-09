<script lang="ts">
	import { onMount, onDestroy, tick, type Snippet } from 'svelte';
	import CodeViewer from './CodeViewer.svelte';
	import { fetchLineRange, normalizePage } from '$lib/fileContent';
	import { nextForwardOffset } from '$lib/pagination';
	import { fileViewPageSize } from '$lib/settingsStore';
	import { highlightFile } from '$lib/highlight';
	import { type LineSelection, isLineLoaded } from '$lib/lineSelection';
	import type { FileResponse } from '$lib/api';

	let {
		source,
		path,
		archivePath = null,
		initialData,
		initialOffset = 0,
		pagedMode = false,
		initialLine = null,
		initialScrollAlign = 'center',
		selection = [],
		wordWrap = false,
		tabWidth = 4,
		header,
		onLineSelect,
		onOverflowChange
	}: {
		source: string;
		path: string;
		archivePath?: string | null;
		/** First page (or whole file when not paged), already fetched by the shell. */
		initialData: FileResponse;
		/** Raw offset `initialData` was fetched at (0 when not paged). */
		initialOffset?: number;
		/** True when the file is larger than the page size and scroll-loading applies. */
		pagedMode?: boolean;
		/** Display line to scroll to after mount (selection target or preserved position). */
		initialLine?: number | null;
		/** 'center' smooth-centers (selection jump); 'top' aligns instantly (position restore). */
		initialScrollAlign?: 'center' | 'top';
		selection?: LineSelection;
		wordWrap?: boolean;
		tabWidth?: number;
		/** Content rendered inside the scroll container above the code table. */
		header?: Snippet;
		onLineSelect?: (selection: LineSelection) => void;
		onOverflowChange?: (hasOverflow: boolean) => void;
	} = $props();

	let highlightedCode = $state('');
	/** Maps 0-based render index → line_number */
	let lineOffsets: number[] = $state([]);
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
	let loadingForward = $state(false);
	let loadingBackward = $state(false);
	let noMoreForward = $state(false);
	let noMoreBackward = $state(true);

	/** Reference to the scrollable .code-container element. */
	let codeContainer: HTMLElement | undefined = $state();

	let codeLines = $derived(highlightedCode ? highlightedCode.split('\n') : []);

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

	/**
	 * Append a newly-loaded forward page without re-highlighting the whole
	 * accumulated buffer. Highlighting only the new lines and concatenating
	 * the HTML keeps each `loadForward` call O(page size) instead of
	 * O(total lines loaded so far) — re-tokenizing everything on every page
	 * turns a long scroll session into O(n²) work. A token that spans the
	 * page boundary (e.g. a multi-line comment) can lose its highlight class
	 * on the seam line, same as it already does at every line boundary today
	 * since each line is rendered as its own `{@html}` fragment.
	 */
	async function appendCodeState(newLines: string[]) {
		lineOffsets = allLineOffsets;
		if (newLines.length === 0) return;
		const newHtml = await highlightFile(newLines, path);
		highlightedCode = highlightedCode ? `${highlightedCode}\n${newHtml}` : newHtml;
	}

	/** Same as {@link appendCodeState}, but prepends — used by `loadBackward`. */
	async function prependCodeState(newLines: string[]) {
		lineOffsets = allLineOffsets;
		if (newLines.length === 0) return;
		const newHtml = await highlightFile(newLines, path);
		highlightedCode = highlightedCode ? `${newHtml}\n${highlightedCode}` : newHtml;
	}

	/** Replace all accumulated state with a single page. */
	async function applyPage(data: FileResponse, offset: number) {
		const page = normalizePage(data, offset);
		allContentLines = [...page.lines];
		allLineOffsets = page.lineOffsets;
		totalLines = data.total_lines;
		if (pagedMode) {
			const pageSize = $fileViewPageSize;
			forwardOffset = nextForwardOffset(offset, pageSize, totalLines);
			backwardOffset = offset;
			noMoreForward = forwardOffset >= totalLines;
			noMoreBackward = offset === 0;
		} else {
			noMoreForward = true;
			noMoreBackward = true;
		}
		lineOffsets = allLineOffsets;
		highlightedCode = await highlightFile(allContentLines, path);
	}

	async function loadForward() {
		if (loadingForward || noMoreForward) return;
		loadingForward = true;
		try {
			const pageSize = $fileViewPageSize;
			const page = await fetchLineRange(source, path, archivePath, forwardOffset, pageSize);
			allContentLines = [...allContentLines, ...page.lines];
			allLineOffsets = [...allLineOffsets, ...page.lineOffsets];
			forwardOffset = nextForwardOffset(forwardOffset, pageSize, totalLines);
			noMoreForward = forwardOffset >= totalLines;
			await appendCodeState(page.lines);
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
			const page = await fetchLineRange(source, path, archivePath, prevOffset, limit);

			// Preserve scroll position when prepending.
			const oldScrollHeight = codeContainer.scrollHeight;
			const oldScrollTop = codeContainer.scrollTop;

			allContentLines = [...page.lines, ...allContentLines];
			allLineOffsets = [...page.lineOffsets, ...allLineOffsets];
			backwardOffset = prevOffset;
			noMoreBackward = prevOffset === 0;
			await prependCodeState(page.lines);

			await tick();
			codeContainer.scrollTop = oldScrollTop + (codeContainer.scrollHeight - oldScrollHeight);
		} catch { /* silent */ }
		loadingBackward = false;
	}

	function scrollToLine(ln: number, align: 'center' | 'top', smooth = true) {
		const el = codeContainer?.querySelector(`#line-${ln}`);
		if (!el) return;
		if (align === 'center') el.scrollIntoView({ behavior: smooth ? 'smooth' : 'auto', block: 'center' });
		else el.scrollIntoView({ behavior: 'auto', block: 'start' });
	}

	/**
	 * Jump to a display line (Ctrl+G / hash edit, plan 062): scroll if it's
	 * within the loaded range, otherwise re-anchor on the page containing it —
	 * same behavior the pre-092 FileViewer implemented inline.
	 */
	export async function jumpToLine(line: number, align: 'center' | 'top' = 'center') {
		if (isLineLoaded(lineOffsets, line)) {
			scrollToLine(line, align);
			return;
		}
		if (!pagedMode) return;
		try {
			const pageSize = $fileViewPageSize;
			const anchor = Math.max(0, Math.floor((line - 1) / pageSize) * pageSize);
			const r = await fetchLineRange(source, path, archivePath, anchor, pageSize);
			await applyPage(r.data, anchor);
			await tick();
			// Instant, not smooth: a smooth animation across the re-anchored page
			// passes through the near-edge zones, whose auto-load scroll
			// compensation cancels it mid-flight.
			scrollToLine(line, align, false);
		} catch { /* silent — jump can be retried */ }
	}

	/**
	 * Display line number of the first row visible in the viewport, or null
	 * before content renders. Used by the shell to preserve position across
	 * a word-wrap toggle or reload.
	 */
	export function getTopLine(): number | null {
		if (!codeContainer) return null;
		const rows = codeContainer.querySelectorAll<HTMLElement>('.code-row');
		if (rows.length === 0) return null;
		const containerTop = codeContainer.getBoundingClientRect().top;
		// Rows are in document order: binary-search the first whose bottom
		// edge is below the container top.
		let lo = 0;
		let hi = rows.length - 1;
		let ans = rows.length - 1;
		while (lo <= hi) {
			const mid = (lo + hi) >> 1;
			if (rows[mid].getBoundingClientRect().bottom > containerTop) {
				ans = mid;
				hi = mid - 1;
			} else {
				lo = mid + 1;
			}
		}
		return lineOffsets[ans] ?? null;
	}

	// Report horizontal overflow so the shell can show the word-wrap toggle.
	let overflowObserver: ResizeObserver | null = null;

	function checkOverflow() {
		if (!codeContainer) return;
		onOverflowChange?.(codeContainer.scrollWidth > codeContainer.clientWidth);
	}

	$effect(() => {
		void [codeLines, wordWrap];
		if (!codeContainer) return;
		checkOverflow();
		if (!overflowObserver) {
			overflowObserver = new ResizeObserver(checkOverflow);
			overflowObserver.observe(codeContainer);
		}
	});

	onMount(async () => {
		const pageSize = $fileViewPageSize;
		let data = initialData;
		let offset = initialOffset;
		// When asked to start at a line the shell's page doesn't cover (e.g. a
		// word-wrap toggle after scrolling far away), fetch the page anchored
		// at that line instead.
		if (pagedMode && initialLine !== null && pageSize > 0) {
			const anchor = Math.max(0, Math.floor((initialLine - 1) / pageSize) * pageSize);
			if (anchor !== initialOffset) {
				try {
					const r = await fetchLineRange(source, path, archivePath, anchor, pageSize);
					data = r.data;
					offset = anchor;
				} catch { /* fall back to the shell's page */ }
			}
		}
		await applyPage(data, offset);
		if (initialLine !== null) {
			await tick();
			scrollToLine(initialLine, initialScrollAlign);
		}
	});

	onDestroy(() => {
		overflowObserver?.disconnect();
	});
</script>

<div class="code-container" bind:this={codeContainer} onscroll={handleScroll}>
	{#if pagedMode && !noMoreBackward}
		<div class="load-sentinel">
			{#if loadingBackward}
				<span class="sentinel-msg">Loading earlier lines…</span>
			{:else}
				<button class="sentinel-btn" onclick={loadBackward}>Load earlier lines</button>
			{/if}
		</div>
	{/if}
	{@render header?.()}
	<CodeViewer
		{codeLines}
		{lineOffsets}
		{selection}
		{wordWrap}
		{tabWidth}
		onLineSelect={(next) => onLineSelect?.(next)}
	/>
	{#if pagedMode && !noMoreForward}
		<div class="load-sentinel">
			{#if loadingForward}
				<span class="sentinel-msg">Loading…</span>
			{/if}
		</div>
	{/if}
</div>

<style>
	.code-container {
		flex: 1;
		overflow: auto;
		background: var(--bg);
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

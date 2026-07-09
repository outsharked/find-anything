<script lang="ts">
	import { onMount, tick, type Snippet } from 'svelte';
	import CodeViewer from './CodeViewer.svelte';
	import { fetchLineRange, normalizePage } from '$lib/fileContent';
	import { fileViewPageSize } from '$lib/settingsStore';
	import { highlightFile } from '$lib/highlight';
	import { type LineSelection } from '$lib/lineSelection';
	import type { FileResponse } from '$lib/api';
	import {
		computeWindowOffset,
		shouldRefetchWindow,
		lineToPixelOffset,
		pixelOffsetToLine,
		fillLineGaps,
		type LinePage
	} from '$lib/virtualWindow';

	let {
		source,
		path,
		archivePath = null,
		initialData,
		initialOffset = 0,
		initialLine = null,
		initialScrollAlign = 'center',
		selection = [],
		tabWidth = 4,
		header,
		onLineSelect,
		onOverflowChange
	}: {
		source: string;
		path: string;
		archivePath?: string | null;
		/** First page, already fetched by the shell (always paged on this path). */
		initialData: FileResponse;
		/** Raw offset `initialData` was fetched at. */
		initialOffset?: number;
		/** Display line to position on after mount (selection target or preserved position). */
		initialLine?: number | null;
		/** 'center' centers the line in the viewport; 'top' aligns it to the top. */
		initialScrollAlign?: 'center' | 'top';
		selection?: LineSelection;
		tabWidth?: number;
		/** Content rendered inside the scroll container above the code table. */
		header?: Snippet;
		onLineSelect?: (selection: LineSelection) => void;
		onOverflowChange?: (hasOverflow: boolean) => void;
	} = $props();

	/** Lines beyond the viewport that must stay covered before a refetch triggers. */
	const OVERSCAN = 200;
	/** Debounce for scroll-driven window fetches. */
	const SCROLL_SETTLE_MS = 100;

	// ── Window state ─────────────────────────────────────────────────────────────
	// All geometry is in raw-line space (0-based, one row per raw line — the
	// rendered window is gap-filled so this invariant always holds).

	/** Raw start of the rendered window (inclusive). */
	let windowStart = $state(0);
	/** Raw end of the rendered window (exclusive). */
	let windowEnd = $state(0);
	let totalLines = $state(0);
	let highlightedCode = $state('');
	/** Maps 0-based render index → line_number (contiguous within the window). */
	let lineOffsets: number[] = $state([]);
	/** Measured height of one code row; 0 until the first window renders. */
	let rowHeightPx = $state(0);
	/** Pixel distance from the scroll container's content top to the table top. */
	let contentOffsetPx = $state(0);
	/** True while a scroll-driven window fetch is in flight (stale-dim). */
	let fetching = $state(false);

	let codeContainer: HTMLElement | undefined = $state();

	let codeLines = $derived(highlightedCode ? highlightedCode.split('\n') : []);
	let spacerBeforePx = $derived(rowHeightPx > 0 ? windowStart * rowHeightPx : 0);
	let spacerAfterPx = $derived(rowHeightPx > 0 ? Math.max(0, totalLines - windowEnd) * rowHeightPx : 0);

	let windowSize = $derived($fileViewPageSize);

	/**
	 * Swap in a new window. The highlight is awaited *before* any state is
	 * touched so rows, line numbers, and spacers all change in one render —
	 * total scroll height is invariant, so scrollTop needs no compensation.
	 */
	async function applyWindow(page: LinePage, rangeStart: number, rangeEnd: number) {
		const filled = fillLineGaps(page.lines, page.lineOffsets, rangeStart, rangeEnd);
		const html = await highlightFile(filled.lines, path);
		windowStart = rangeStart;
		windowEnd = rangeEnd;
		lineOffsets = filled.lineOffsets;
		highlightedCode = html;
	}

	// Monotonic generation: a newer fetch supersedes any still in flight.
	let fetchGeneration = 0;

	async function fetchWindow(start: number) {
		const gen = ++fetchGeneration;
		fetching = true;
		try {
			const end = Math.min(start + windowSize, totalLines);
			const page = await fetchLineRange(source, path, archivePath, start, end - start);
			if (gen !== fetchGeneration) return;
			await applyWindow(page, start, end);
		} catch { /* silent — the next scroll retries */ }
		if (gen === fetchGeneration) fetching = false;
	}

	// ── Measurement ──────────────────────────────────────────────────────────────

	function measure(): boolean {
		if (!codeContainer) return false;
		const row = codeContainer.querySelector('.code-row');
		const table = codeContainer.querySelector('.code-table');
		if (!row || !table) return false;
		const h = row.getBoundingClientRect().height;
		if (h <= 0) return false;
		rowHeightPx = h;
		contentOffsetPx =
			table.getBoundingClientRect().top -
			codeContainer.getBoundingClientRect().top +
			codeContainer.scrollTop;
		return true;
	}

	/** Re-measure after a container resize (zoom/font change), preserving the top line. */
	function remeasure() {
		if (!codeContainer || rowHeightPx <= 0) return;
		const topRaw = pixelOffsetToLine(codeContainer.scrollTop, rowHeightPx, contentOffsetPx);
		const prevHeight = rowHeightPx;
		if (!measure()) return;
		if (rowHeightPx !== prevHeight) {
			codeContainer.scrollTop = lineToPixelOffset(topRaw, rowHeightPx, contentOffsetPx);
		}
	}

	// Horizontal overflow gates the shell's word-wrap toggle. Latched: the
	// widest line scrolling out of the window must not hide the toggle again.
	let overflowLatched = false;

	function checkOverflow() {
		if (!codeContainer || overflowLatched) return;
		if (codeContainer.scrollWidth > codeContainer.clientWidth) {
			overflowLatched = true;
			onOverflowChange?.(true);
		}
	}

	$effect(() => {
		void codeLines;
		checkOverflow();
	});

	$effect(() => {
		if (!codeContainer) return;
		const obs = new ResizeObserver(() => {
			checkOverflow();
			remeasure();
		});
		obs.observe(codeContainer);
		return () => obs.disconnect();
	});

	// ── Scrolling ────────────────────────────────────────────────────────────────

	let scrollDebounce: ReturnType<typeof setTimeout> | null = null;

	function handleScroll() {
		if (scrollDebounce !== null) clearTimeout(scrollDebounce);
		scrollDebounce = setTimeout(onScrollSettled, SCROLL_SETTLE_MS);
	}

	function onScrollSettled() {
		scrollDebounce = null;
		if (!codeContainer || rowHeightPx <= 0) return;
		const viewStart = pixelOffsetToLine(codeContainer.scrollTop, rowHeightPx, contentOffsetPx);
		const viewEnd = pixelOffsetToLine(
			codeContainer.scrollTop + codeContainer.clientHeight,
			rowHeightPx,
			contentOffsetPx
		) + 1;
		const refetch = shouldRefetchWindow({
			windowStart,
			windowEnd,
			viewportStartLine: viewStart,
			viewportEndLine: viewEnd,
			overscan: OVERSCAN,
			totalLines
		});
		if (refetch) {
			const center = Math.floor((viewStart + viewEnd) / 2);
			fetchWindow(computeWindowOffset(center, windowSize, totalLines));
		}
	}

	function scrollToRaw(raw: number, align: 'center' | 'top') {
		if (!codeContainer || rowHeightPx <= 0) return;
		const px = lineToPixelOffset(raw, rowHeightPx, contentOffsetPx);
		codeContainer.scrollTop = align === 'center'
			? Math.max(0, px - (codeContainer.clientHeight - rowHeightPx) / 2)
			: px;
	}

	/** Jump to a display line: fetch a centered window if needed, then position directly. */
	export async function jumpToLine(line: number, align: 'center' | 'top' = 'center') {
		const raw = line - 1;
		if (raw < windowStart || raw >= windowEnd) {
			await fetchWindow(computeWindowOffset(raw, windowSize, totalLines));
		}
		scrollToRaw(raw, align);
	}

	/**
	 * Display line number of the first row visible in the viewport. Used by
	 * the shell to preserve position across a word-wrap toggle or reload.
	 */
	export function getTopLine(): number | null {
		if (!codeContainer || rowHeightPx <= 0) return null;
		return pixelOffsetToLine(codeContainer.scrollTop, rowHeightPx, contentOffsetPx) + 1;
	}

	onMount(async () => {
		totalLines = initialData.total_lines;
		const rangeEnd = Math.min(initialOffset + windowSize, totalLines);
		await applyWindow(normalizePage(initialData, initialOffset), initialOffset, rangeEnd);
		await tick();
		// Measure the rendered rows, which installs the spacers (they derive
		// from rowHeightPx), then position the viewport — the one transition
		// where scroll geometry changes.
		measure();
		await tick();
		if (initialLine !== null) {
			await jumpToLine(initialLine, initialScrollAlign);
		}
	});
</script>

<div class="code-container" bind:this={codeContainer} onscroll={handleScroll}>
	{@render header?.()}
	<div class="window-body" class:fetching>
		<CodeViewer
			{codeLines}
			{lineOffsets}
			{selection}
			wordWrap={false}
			{tabWidth}
			{spacerBeforePx}
			{spacerAfterPx}
			onLineSelect={(next) => onLineSelect?.(next)}
		/>
	</div>
</div>

<style>
	.code-container {
		flex: 1;
		overflow: auto;
		background: var(--bg);
	}

	.window-body {
		transition: opacity 0.2s ease-in-out;
	}

	.window-body.fetching {
		opacity: 0.5;
	}
</style>

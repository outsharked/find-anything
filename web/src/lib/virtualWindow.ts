/**
 * Pure windowing math for the virtualized file viewer (plan 092).
 *
 * All line arguments are **raw-line offsets**: 0-based indexes into the
 * file's content lines, the same space used by `/api/v1/file?offset=`.
 * Display line numbers are raw offset + 1. The server may skip raw lines
 * whose chunk lookup misses (see `nextForwardOffset` in pagination.ts), so
 * rendered rows must be gap-filled via {@link fillLineGaps} to keep the
 * "one raw line = one row" invariant the spacer math depends on.
 */

/**
 * Clamp a window start so `[start, start + windowSize)` stays inside
 * `[0, totalLines)` while centering on `centerLine`.
 */
export function computeWindowOffset(centerLine: number, windowSize: number, totalLines: number): number {
	if (totalLines <= windowSize) return 0;
	const start = centerLine - Math.floor(windowSize / 2);
	return Math.max(0, Math.min(start, totalLines - windowSize));
}

export interface RefetchParams {
	/** Raw start of the loaded window (inclusive). */
	windowStart: number;
	/** Raw end of the loaded window (exclusive). */
	windowEnd: number;
	/** First raw line visible in the viewport. */
	viewportStartLine: number;
	/** Raw line just past the last visible one (exclusive). */
	viewportEndLine: number;
	/** Extra lines beyond the viewport that must still be inside the window. */
	overscan: number;
	totalLines: number;
}

/**
 * Whether the visible range (plus overscan) has escaped the current window.
 * Edges clamp: no refetch is demanded past the start or end of the file.
 */
export function shouldRefetchWindow(p: RefetchParams): boolean {
	if (p.viewportStartLine - p.overscan < p.windowStart && p.windowStart > 0) return true;
	if (p.viewportEndLine + p.overscan > p.windowEnd && p.windowEnd < p.totalLines) return true;
	return false;
}

/** Pixel offset of a raw line's top edge within the scroll container. */
export function lineToPixelOffset(line: number, rowHeight: number, contentOffsetPx: number): number {
	return contentOffsetPx + line * rowHeight;
}

/** Raw line whose row covers the given pixel offset within the scroll container. */
export function pixelOffsetToLine(px: number, rowHeight: number, contentOffsetPx: number): number {
	if (rowHeight <= 0) return 0;
	return Math.max(0, Math.floor((px - contentOffsetPx) / rowHeight));
}

export interface LinePage {
	lines: string[];
	/** 1-based display line numbers, parallel to `lines`. */
	lineOffsets: number[];
}

/**
 * Insert empty placeholder rows for raw lines the server skipped, so the
 * result always has exactly `rangeEnd - rangeStart` rows covering the raw
 * range `[rangeStart, rangeEnd)`. Input `lineOffsets` are 1-based display
 * numbers; entries outside the range are dropped.
 */
export function fillLineGaps(
	lines: string[],
	lineOffsets: number[],
	rangeStart: number,
	rangeEnd: number
): LinePage {
	const byLine = new Map<number, string>();
	for (let i = 0; i < lineOffsets.length; i++) byLine.set(lineOffsets[i], lines[i]);
	const outLines: string[] = [];
	const outOffsets: number[] = [];
	for (let raw = rangeStart; raw < rangeEnd; raw++) {
		const display = raw + 1;
		outLines.push(byLine.get(display) ?? '');
		outOffsets.push(display);
	}
	return { lines: outLines, lineOffsets: outOffsets };
}

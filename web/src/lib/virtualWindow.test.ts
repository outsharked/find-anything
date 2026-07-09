import { describe, it, expect } from 'vitest';
import {
	computeWindowOffset,
	shouldRefetchWindow,
	lineToPixelOffset,
	pixelOffsetToLine,
	fillLineGaps
} from './virtualWindow';
import { normalizePage } from './fileContent';

describe('computeWindowOffset', () => {
	it('centers the window on the target line', () => {
		expect(computeWindowOffset(5000, 2000, 100_000)).toBe(4000);
	});

	it('clamps to 0 near the start of the file', () => {
		expect(computeWindowOffset(100, 2000, 100_000)).toBe(0);
	});

	it('clamps so the window does not run past the end of the file', () => {
		expect(computeWindowOffset(99_950, 2000, 100_000)).toBe(98_000);
	});

	it('returns 0 when the file fits inside one window', () => {
		expect(computeWindowOffset(500, 2000, 1500)).toBe(0);
		expect(computeWindowOffset(0, 2000, 2000)).toBe(0);
	});

	it('centering an early line lands at exactly 0 when target is half a window in', () => {
		expect(computeWindowOffset(1000, 2000, 100_000)).toBe(0);
	});
});

describe('shouldRefetchWindow', () => {
	const base = {
		windowStart: 4000,
		windowEnd: 6000,
		overscan: 200,
		totalLines: 100_000
	};

	it('no refetch when the viewport sits comfortably inside the window', () => {
		expect(shouldRefetchWindow({ ...base, viewportStartLine: 4500, viewportEndLine: 4600 })).toBe(false);
	});

	it('refetch when the viewport approaches the window top', () => {
		expect(shouldRefetchWindow({ ...base, viewportStartLine: 4100, viewportEndLine: 4200 })).toBe(true);
	});

	it('refetch when the viewport approaches the window bottom', () => {
		expect(shouldRefetchWindow({ ...base, viewportStartLine: 5700, viewportEndLine: 5900 })).toBe(true);
	});

	it('exactly at the overscan boundary does not refetch', () => {
		expect(shouldRefetchWindow({ ...base, viewportStartLine: 4200, viewportEndLine: 5800 })).toBe(false);
	});

	it('no refetch past the start of the file when the window is already at 0', () => {
		expect(shouldRefetchWindow({
			...base, windowStart: 0, windowEnd: 2000,
			viewportStartLine: 0, viewportEndLine: 100
		})).toBe(false);
	});

	it('no refetch past the end of the file when the window is already at the end', () => {
		expect(shouldRefetchWindow({
			...base, windowStart: 98_000, windowEnd: 100_000,
			viewportStartLine: 99_900, viewportEndLine: 100_000
		})).toBe(false);
	});
});

describe('pixel/line conversion', () => {
	it('round-trips through a content offset', () => {
		const rowHeight = 20.8;
		const contentOffset = 137;
		const px = lineToPixelOffset(500, rowHeight, contentOffset);
		expect(px).toBeCloseTo(137 + 500 * 20.8);
		expect(pixelOffsetToLine(px, rowHeight, contentOffset)).toBe(500);
	});

	it('pixelOffsetToLine clamps to 0 above the table (inside the header content)', () => {
		expect(pixelOffsetToLine(50, 20.8, 137)).toBe(0);
	});

	it('pixelOffsetToLine returns 0 for a zero row height instead of dividing by zero', () => {
		expect(pixelOffsetToLine(500, 0, 0)).toBe(0);
	});

	it('mid-row pixel maps to the row it falls inside', () => {
		expect(pixelOffsetToLine(20.8 * 3 + 10, 20.8, 0)).toBe(3);
	});
});

describe('fillLineGaps', () => {
	it('passes through a gapless range unchanged', () => {
		const { lines, lineOffsets } = fillLineGaps(['a', 'b', 'c'], [11, 12, 13], 10, 13);
		expect(lines).toEqual(['a', 'b', 'c']);
		expect(lineOffsets).toEqual([11, 12, 13]);
	});

	it('fills a gap in the middle with an empty placeholder row', () => {
		const { lines, lineOffsets } = fillLineGaps(['a', 'c'], [11, 13], 10, 13);
		expect(lines).toEqual(['a', '', 'c']);
		expect(lineOffsets).toEqual([11, 12, 13]);
	});

	it('fills gaps at the start and end of the range', () => {
		const { lines, lineOffsets } = fillLineGaps(['b'], [12], 10, 14);
		expect(lines).toEqual(['', 'b', '', '']);
		expect(lineOffsets).toEqual([11, 12, 13, 14]);
	});

	it('drops input entries outside the requested range', () => {
		const { lines } = fillLineGaps(['x', 'a'], [5, 11], 10, 12);
		expect(lines).toEqual(['a', '']);
	});

	it('an entirely-skipped range renders as all placeholders', () => {
		const { lines, lineOffsets } = fillLineGaps([], [], 0, 3);
		expect(lines).toEqual(['', '', '']);
		expect(lineOffsets).toEqual([1, 2, 3]);
	});
});

describe('normalizePage', () => {
	it('adjusts server line_offsets (content starts at 2) to display numbers', () => {
		const page = normalizePage({ lines: ['a', 'b'], line_offsets: [2, 4] }, 0);
		expect(page.lineOffsets).toEqual([1, 3]);
	});

	it('falls back to contiguous numbering from the fetch offset', () => {
		const page = normalizePage({ lines: ['a', 'b', 'c'] }, 100);
		expect(page.lineOffsets).toEqual([101, 102, 103]);
	});

	it('empty line_offsets also falls back to contiguous numbering', () => {
		const page = normalizePage({ lines: ['a'], line_offsets: [] }, 0);
		expect(page.lineOffsets).toEqual([1]);
	});
});

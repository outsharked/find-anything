/** A single line number or an inclusive [start, end] range. */
export type LinePart = number | [number, number];

/** Ordered list of selected lines/ranges. Empty = no selection. */
export type LineSelection = LinePart[];

/**
 * Parse a URL hash string like "#L43", "#L20-30", "#L20-30,43" into a LineSelection.
 * Returns [] for any unrecognised input.
 */
export function parseHash(hash: string): LineSelection {
	const body = hash.startsWith('#L') ? hash.slice(2) : hash.startsWith('L') ? hash.slice(1) : '';
	if (!body) return [];
	const parts: LineSelection = [];
	for (const token of body.split(',')) {
		const dash = token.indexOf('-');
		if (dash > 0) {
			const start = parseInt(token.slice(0, dash), 10);
			const end = parseInt(token.slice(dash + 1), 10);
			if (!isNaN(start) && !isNaN(end) && start > 0 && end >= start) {
				parts.push([start, end]);
			}
		} else {
			const n = parseInt(token, 10);
			if (!isNaN(n) && n > 0) parts.push(n);
		}
	}
	return parts;
}

/**
 * Serialise a LineSelection back to a hash string like "#L20-30,43".
 * Returns "" when selection is empty.
 */
export function formatHash(sel: LineSelection): string {
	if (sel.length === 0) return '';
	const tokens = sel.map((p) => (Array.isArray(p) ? `${p[0]}-${p[1]}` : String(p)));
	return '#L' + tokens.join(',');
}

/**
 * Expand a LineSelection into a Set<number> for O(1) membership tests.
 * Ranges are capped at 10 000 lines to avoid performance issues.
 */
export function selectionSet(sel: LineSelection): Set<number> {
	const s = new Set<number>();
	for (const part of sel) {
		if (Array.isArray(part)) {
			const end = Math.min(part[1], part[0] + 10_000);
			for (let i = part[0]; i <= end; i++) s.add(i);
		} else {
			s.add(part);
		}
	}
	return s;
}

/** Returns the first (lowest) line number in the selection, or null if empty. */
export function firstLine(sel: LineSelection): number | null {
	if (sel.length === 0) return null;
	const first = sel[0];
	return Array.isArray(first) ? first[0] : first;
}

/**
 * Whether line `ln` falls within the span of currently-rendered lines.
 *
 * `lineOffsets` is sorted but may have gaps (e.g. blank lines with no stored
 * content chunk never appear in it), so this checks the covered *range*
 * rather than exact membership — a line inside the loaded span that happens
 * to be a gap simply won't have a DOM node to scroll to, which is an
 * acceptable edge case compared to re-fetching a page that's already loaded.
 */
export function isLineLoaded(lineOffsets: number[], ln: number): boolean {
	return lineOffsets.length > 0 && ln >= lineOffsets[0] && ln <= lineOffsets[lineOffsets.length - 1];
}

/**
 * Toggle a single line number in the selection.
 * Handles simple number parts only (ranges are kept as-is).
 */
export function toggleLine(sel: LineSelection, line: number): LineSelection {
	const idx = sel.findIndex((p) => !Array.isArray(p) && p === line);
	if (idx >= 0) {
		return sel.filter((_, i) => i !== idx);
	}
	return [...sel, line];
}

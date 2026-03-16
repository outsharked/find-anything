import { describe, it, expect } from 'vitest';
import { parseHash, formatHash, selectionSet, firstLine, toggleLine } from './lineSelection';

// ── parseHash ────────────────────────────────────────────────────────────────

describe('parseHash', () => {
	it('single line "#L43" → [43]', () => {
		expect(parseHash('#L43')).toEqual([43]);
	});

	it('range "#L20-30" → [[20, 30]]', () => {
		expect(parseHash('#L20-30')).toEqual([[20, 30]]);
	});

	it('mixed "#L20-30,43" → [[20, 30], 43]', () => {
		expect(parseHash('#L20-30,43')).toEqual([[20, 30], 43]);
	});

	it('no hash prefix "L43" → [43]', () => {
		expect(parseHash('L43')).toEqual([43]);
	});

	it('empty string "" → []', () => {
		expect(parseHash('')).toEqual([]);
	});

	it('garbage "#Lxyz" → []', () => {
		expect(parseHash('#Lxyz')).toEqual([]);
	});

	it('invalid range end < start "#L30-20" → []', () => {
		expect(parseHash('#L30-20')).toEqual([]);
	});
});

// ── formatHash ───────────────────────────────────────────────────────────────

describe('formatHash', () => {
	it('empty [] → ""', () => {
		expect(formatHash([])).toBe('');
	});

	it('single line [43] → "#L43"', () => {
		expect(formatHash([43])).toBe('#L43');
	});

	it('range [[20, 30]] → "#L20-30"', () => {
		expect(formatHash([[20, 30]])).toBe('#L20-30');
	});

	it('mixed [[20, 30], 43] → "#L20-30,43"', () => {
		expect(formatHash([[20, 30], 43])).toBe('#L20-30,43');
	});

	it('round-trip: formatHash(parseHash("#L10-20,5")) → "#L10-20,5"', () => {
		expect(formatHash(parseHash('#L10-20,5'))).toBe('#L10-20,5');
	});
});

// ── selectionSet ─────────────────────────────────────────────────────────────

describe('selectionSet', () => {
	it('single number → set contains that number, not adjacent numbers', () => {
		const s = selectionSet([5]);
		expect(s.has(5)).toBe(true);
		expect(s.has(4)).toBe(false);
		expect(s.has(6)).toBe(false);
	});

	it('range → set contains all numbers in range', () => {
		const s = selectionSet([[3, 7]]);
		expect(s.has(3)).toBe(true);
		expect(s.has(5)).toBe(true);
		expect(s.has(7)).toBe(true);
		expect(s.has(2)).toBe(false);
		expect(s.has(8)).toBe(false);
	});

	it('range cap: [[1, 15000]] → set size is 10_001 (capped at start + 10_000)', () => {
		const s = selectionSet([[1, 15000]]);
		expect(s.size).toBe(10_001);
		expect(s.has(1)).toBe(true);
		expect(s.has(10_001)).toBe(true);
		expect(s.has(10_002)).toBe(false);
	});

	it('empty [] → empty set', () => {
		const s = selectionSet([]);
		expect(s.size).toBe(0);
	});
});

// ── firstLine ─────────────────────────────────────────────────────────────────

describe('firstLine', () => {
	it('single number [43] → 43', () => {
		expect(firstLine([43])).toBe(43);
	});

	it('range first: [[20, 30], 43] → 20', () => {
		expect(firstLine([[20, 30], 43])).toBe(20);
	});

	it('empty [] → null', () => {
		expect(firstLine([])).toBeNull();
	});
});

// ── toggleLine ────────────────────────────────────────────────────────────────

describe('toggleLine', () => {
	it('add a line not present → line appears in result', () => {
		const result = toggleLine([1, 2], 5);
		expect(selectionSet(result).has(5)).toBe(true);
	});

	it('remove a line that is present → line gone from result', () => {
		const result = toggleLine([1, 5, 10], 5);
		expect(selectionSet(result).has(5)).toBe(false);
	});

	it('range parts are kept unchanged when toggling an unrelated number', () => {
		const sel = [[20, 30] as [number, number], 5];
		const result = toggleLine(sel, 99);
		const rangepart = result.find((p) => Array.isArray(p));
		expect(rangepart).toEqual([20, 30]);
		expect(selectionSet(result).has(99)).toBe(true);
	});
});

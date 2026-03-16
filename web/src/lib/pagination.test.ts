import { describe, it, expect } from 'vitest';
import { mergePage } from './pagination';
import type { SearchResult } from './api';

// Minimal stub factory — only the fields used by mergePage's dedup key.
function makeResult(
	source: string,
	path: string,
	line_number: number,
	archive_path: string | null = null
): SearchResult {
	return {
		source,
		path,
		archive_path,
		line_number,
		snippet: '',
		score: 1,
		kind: 'text',
		mtime: 0,
		size: null,
		context_lines: []
	};
}

describe('mergePage', () => {
	it('no duplicates — all incoming items are added and offset advances by incoming.length', () => {
		const existing = [makeResult('s', 'a.txt', 1)];
		const incoming = [makeResult('s', 'b.txt', 1), makeResult('s', 'c.txt', 1)];
		const { results, newOffset } = mergePage(existing, incoming, 10);
		expect(results).toHaveLength(3);
		expect(results[1].path).toBe('b.txt');
		expect(results[2].path).toBe('c.txt');
		expect(newOffset).toBe(12); // 10 + incoming.length(2)
	});

	it('all duplicates — no items added, offset still advances by incoming.length', () => {
		const existing = [makeResult('s', 'a.txt', 1), makeResult('s', 'b.txt', 2)];
		const incoming = [makeResult('s', 'a.txt', 1), makeResult('s', 'b.txt', 2)];
		const { results, newOffset } = mergePage(existing, incoming, 5);
		expect(results).toHaveLength(2);
		expect(newOffset).toBe(7); // 5 + incoming.length(2)
	});

	it('partial duplicates — only fresh items added, offset advances by full incoming.length', () => {
		const existing = [makeResult('s', 'a.txt', 1)];
		const incoming = [makeResult('s', 'a.txt', 1), makeResult('s', 'new.txt', 3)];
		const { results, newOffset } = mergePage(existing, incoming, 20);
		expect(results).toHaveLength(2);
		expect(results[1].path).toBe('new.txt');
		// offset advances by 2 (full incoming), NOT by 1 (fresh count)
		expect(newOffset).toBe(22);
	});

	it('empty incoming — results unchanged, offset advances by 0', () => {
		const existing = [makeResult('s', 'a.txt', 1)];
		const { results, newOffset } = mergePage(existing, [], 8);
		expect(results).toHaveLength(1);
		expect(newOffset).toBe(8);
	});

	it('empty existing — first page load, all incoming items added', () => {
		const incoming = [makeResult('s', 'a.txt', 1), makeResult('s', 'b.txt', 2)];
		const { results, newOffset } = mergePage([], incoming, 0);
		expect(results).toHaveLength(2);
		expect(newOffset).toBe(2);
	});

	it('archive_path distinguishes items with same source/path/line_number', () => {
		const existing = [makeResult('s', 'outer.zip', 1, 'outer.zip::a.txt')];
		const incoming = [
			makeResult('s', 'outer.zip', 1, 'outer.zip::a.txt'), // duplicate
			makeResult('s', 'outer.zip', 1, 'outer.zip::b.txt') // different archive_path → fresh
		];
		const { results, newOffset } = mergePage(existing, incoming, 0);
		expect(results).toHaveLength(2);
		expect(results[1].archive_path).toBe('outer.zip::b.txt');
		expect(newOffset).toBe(2);
	});

	it('archive_path=null dedup — two items with same key and null archive_path are deduped', () => {
		const existing = [makeResult('s', 'file.txt', 5, null)];
		const incoming = [makeResult('s', 'file.txt', 5, null)]; // same key
		const { results, newOffset } = mergePage(existing, incoming, 3);
		expect(results).toHaveLength(1);
		expect(newOffset).toBe(4);
	});
});

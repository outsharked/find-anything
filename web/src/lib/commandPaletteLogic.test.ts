import { describe, it, expect } from 'vitest';
import {
	buildItems,
	filterItems,
	fuzzyScore,
	displayPath,
	archivePathOf,
	splitDisplayPath,
} from './commandPaletteLogic';
import type { FileRecord } from '$lib/api';

const file = (path: string, kind = 'text'): FileRecord => ({ path, kind, mtime: 0 });

// ── buildItems ────────────────────────────────────────────────────────────────
//
// Regression: CommandPalette always showed "No matches" after the plan-041
// multi-source refactor because the cache was declared `const`. Svelte's
// reactivity is assignment-based, so `cache.set(...)` never triggered the
// `allItems` reactive statement to re-run — the palette loaded files into the
// Map but the UI remained empty. Fix: declare `let cache` and add
// `cache = cache` after each mutation. buildItems() is the pure function that
// reactive statement calls; these tests pin the expected behaviour.

describe('buildItems', () => {
	it('returns empty array when cache has no entry for the requested source', () => {
		const cache = new Map<string, FileRecord[]>();
		expect(buildItems(cache, ['mysource'])).toEqual([]);
	});

	it('returns items tagged with their source once cache is populated', () => {
		const cache = new Map([['src', [file('foo/bar.ts')]]]);
		const items = buildItems(cache, ['src']);
		expect(items).toHaveLength(1);
		expect(items[0].path).toBe('foo/bar.ts');
		expect(items[0].source).toBe('src');
	});

	it('merges items from multiple sources in order', () => {
		const cache = new Map([
			['a', [file('a/one.ts'), file('a/two.ts')]],
			['b', [file('b/three.ts')]],
		]);
		const items = buildItems(cache, ['a', 'b']);
		expect(items).toHaveLength(3);
		expect(items.map((i) => i.source)).toEqual(['a', 'a', 'b']);
	});

	it('skips sources not yet in the cache (still loading)', () => {
		const cache = new Map([['loaded', [file('x.ts')]]]);
		const items = buildItems(cache, ['loaded', 'pending']);
		expect(items).toHaveLength(1);
		expect(items[0].source).toBe('loaded');
	});

	it('returns empty for an empty sources list', () => {
		const cache = new Map([['src', [file('x.ts')]]]);
		expect(buildItems(cache, [])).toEqual([]);
	});
});

// ── filterItems ───────────────────────────────────────────────────────────────

describe('filterItems', () => {
	const items = [
		{ path: 'src/main.rs', kind: 'text', mtime: 0, source: 's' },
		{ path: 'src/lib.rs', kind: 'text', mtime: 0, source: 's' },
		{ path: 'docs/readme.md', kind: 'text', mtime: 0, source: 's' },
	];

	it('with empty query returns first 50 items scored 0', () => {
		const result = filterItems(items, '');
		expect(result).toHaveLength(3);
		expect(result.every((r) => r.score === 0)).toBe(true);
	});

	it('filters out items that do not match the query', () => {
		const result = filterItems(items, 'readme');
		expect(result).toHaveLength(1);
		expect(result[0].path).toBe('docs/readme.md');
	});

	it('sorts results by descending score', () => {
		// 'main' matches src/main.rs in the filename → higher score than src/lib.rs
		const result = filterItems(items, 'main');
		expect(result[0].path).toBe('src/main.rs');
	});

	it('caps results at 50', () => {
		const many = Array.from({ length: 100 }, (_, i) => ({
			path: `file${i}.ts`,
			kind: 'text',
			mtime: 0,
			source: 's',
		}));
		expect(filterItems(many, '').length).toBe(50);
		expect(filterItems(many, 'file').length).toBe(50);
	});
});

// ── fuzzyScore ────────────────────────────────────────────────────────────────

describe('fuzzyScore', () => {
	it('returns 0 for empty query', () => {
		expect(fuzzyScore('', 'anything')).toBe(0);
	});

	it('returns -1 when query is not a subsequence of path', () => {
		expect(fuzzyScore('xyz', 'abc')).toBe(-1);
	});

	it('gives exact substring match a high score', () => {
		expect(fuzzyScore('main', 'src/main.rs')).toBeGreaterThanOrEqual(100);
	});

	it('gives higher score when match is in filename vs directory', () => {
		const inFilename = fuzzyScore('lib', 'src/lib.rs');
		const inDir = fuzzyScore('lib', 'lib/something_else.rs');
		expect(inFilename).toBeGreaterThan(inDir);
	});

	it('gives highest score when query matches start of filename', () => {
		const startMatch = fuzzyScore('mai', 'src/main.rs');
		const midMatch = fuzzyScore('ain', 'src/main.rs');
		expect(startMatch).toBeGreaterThan(midMatch);
	});

	it('is case-insensitive', () => {
		expect(fuzzyScore('MAIN', 'src/main.rs')).toBe(fuzzyScore('main', 'src/main.rs'));
	});

	it('falls back to subsequence scoring when no substring match', () => {
		// 'm', 'a', 'i', 'n' all appear in order in 'my_app_in_rust'
		expect(fuzzyScore('main', 'my_app_in_rust')).toBeGreaterThanOrEqual(0);
	});
});

// ── displayPath ───────────────────────────────────────────────────────────────

describe('displayPath', () => {
	it('returns the path unchanged for plain files', () => {
		expect(displayPath('src/main.rs')).toBe('src/main.rs');
	});

	it('formats composite paths as "zip → member"', () => {
		expect(displayPath('archive.zip::member.txt')).toBe('archive.zip → member.txt');
	});

	it('handles nested composite paths (only first :: is the separator)', () => {
		expect(displayPath('outer.zip::inner.zip::file.txt')).toBe('outer.zip → inner.zip::file.txt');
	});
});

// ── splitDisplayPath ──────────────────────────────────────────────────────────

describe('splitDisplayPath', () => {
	it('splits plain paths into name and dir', () => {
		expect(splitDisplayPath('/home/user/file.txt')).toEqual({
			name: 'file.txt',
			dir: '/home/user',
		});
	});

	it('returns name with empty dir for a bare filename', () => {
		expect(splitDisplayPath('file.txt')).toEqual({ name: 'file.txt', dir: '' });
	});

	it('uses the inner member as name for a single-level archive member', () => {
		expect(splitDisplayPath('/home/user/archive.zip::member.txt')).toEqual({
			name: 'member.txt',
			dir: '/home/user/archive.zip',
		});
	});

	it('uses the terminal member as name for nested archive members', () => {
		// outer.zip::c.tar::file.txt → name=file.txt, dir=outer.zip::c.tar
		expect(splitDisplayPath('/home/user/outer.zip::c.tar::file.txt')).toEqual({
			name: 'file.txt',
			dir: '/home/user/outer.zip::c.tar',
		});
	});

	it('handles a slash inside the archive member path', () => {
		// archive.zip::subdir/file.txt → last sep is /, name=file.txt
		expect(splitDisplayPath('/home/user/archive.zip::subdir/file.txt')).toEqual({
			name: 'file.txt',
			dir: '/home/user/archive.zip::subdir',
		});
	});
});

// ── archivePathOf ─────────────────────────────────────────────────────────────

describe('archivePathOf', () => {
	it('returns null for plain paths', () => {
		expect(archivePathOf('src/main.rs')).toBeNull();
	});

	it('returns the member portion of a composite path', () => {
		expect(archivePathOf('archive.zip::member.txt')).toBe('member.txt');
	});

	it('returns everything after the first :: for nested paths', () => {
		expect(archivePathOf('outer.zip::inner.zip::file.txt')).toBe('inner.zip::file.txt');
	});
});

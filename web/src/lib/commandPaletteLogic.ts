import type { FileRecord } from '$lib/api';

export type SourcedFile = FileRecord & { source: string };

/**
 * Build the flat item list from the per-source cache.
 *
 * The cache is a plain Map populated asynchronously by loadAll(). In the
 * Svelte component this function is called from a reactive statement, so the
 * caller is responsible for triggering reactivity after mutating the Map
 * (i.e. `cache = cache` after `cache.set(...)`). If the cache Map is declared
 * `const`, Svelte never re-runs the reactive statement and this always returns
 * an empty array — which was the Ctrl+P "No matches" regression (fixed by
 * declaring `let cache` in CommandPalette.svelte).
 */
export function buildItems(cache: Map<string, FileRecord[]>, sources: string[]): SourcedFile[] {
	return sources.flatMap((s) =>
		(cache.get(s) ?? []).map((f): SourcedFile => ({ ...f, source: s }))
	);
}

/**
 * Filter and rank items against a query string.
 * Returns at most 50 results, sorted by descending score.
 * With an empty query, returns the first 50 items unscored.
 */
export function filterItems(
	items: SourcedFile[],
	query: string
): (SourcedFile & { score: number })[] {
	if (!query) return items.slice(0, 50).map((f) => ({ ...f, score: 0 }));
	return items
		.map((f) => ({ ...f, score: fuzzyScore(query, f.path) }))
		.filter((f) => f.score >= 0)
		.sort((a, b) => b.score - a.score)
		.slice(0, 50);
}

/**
 * Simple character-subsequence fuzzy scorer with exact match boosting.
 * Returns -1 when the query is not a subsequence of path (no match).
 */
export function fuzzyScore(q: string, path: string): number {
	if (!q) return 0;
	const ql = q.toLowerCase();
	const pl = path.toLowerCase();

	if (pl.includes(ql)) {
		let bonus = 100;
		const lastSlash = Math.max(pl.lastIndexOf('/'), pl.lastIndexOf('::'));
		const filename = lastSlash >= 0 ? pl.slice(lastSlash + 1) : pl;
		if (filename.includes(ql)) bonus += 50;
		if (filename.startsWith(ql)) bonus += 50;
		return bonus;
	}

	let qi = 0;
	let score = 0;
	let lastMatch = -1;
	for (let pi = 0; pi < pl.length && qi < ql.length; pi++) {
		if (pl[pi] === ql[qi]) {
			if (pi === lastMatch + 1) score += 2;
			if (pi === 0 || '/-_.'.includes(pl[pi - 1])) score += 3;
			lastMatch = pi;
			qi++;
		}
	}
	return qi === ql.length ? score : -1;
}

/** Display label: archive members shown as "zip → member". */
export function displayPath(path: string): string {
	const i = path.indexOf('::');
	if (i < 0) return path;
	return `${path.slice(0, i)} → ${path.slice(i + 2)}`;
}

/**
 * Split a path into { name, dir } for VS Code-style display:
 * filename prominent on the left, directory dimmed on the right.
 *
 * Always shows the terminal filename (last segment after any `::` or `/`) as
 * `name`, and everything before it as `dir`. For nested archive members like
 * `outer.zip::c.tar::file.txt`, `name` = `file.txt` and `dir` = `outer.zip::c.tar`.
 */
export function splitDisplayPath(path: string): { name: string; dir: string } {
	const lastDoubleColon = path.lastIndexOf('::');
	const lastSlash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));

	if (lastDoubleColon >= 0 && lastDoubleColon > lastSlash) {
		return { name: path.slice(lastDoubleColon + 2), dir: path.slice(0, lastDoubleColon) };
	}
	if (lastSlash >= 0) {
		return { name: path.slice(lastSlash + 1), dir: path.slice(0, lastSlash) };
	}
	return { name: path, dir: '' };
}

/** For a composite path, returns the member portion; null for plain paths. */
export function archivePathOf(path: string): string | null {
	const i = path.indexOf('::');
	return i >= 0 ? path.slice(i + 2) : null;
}

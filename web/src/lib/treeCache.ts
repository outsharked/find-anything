import { listDir } from '$lib/api';
import type { DirEntry } from '$lib/api';

// Cache of directory listings, keyed by `${source}:${prefix}`.
//
// Populated two ways:
//   1. prefetchTreePath() — called fire-and-forget from handlePaletteSelect
//      before the tree renders, so all intermediate levels are ready in
//      parallel instead of being fetched one-at-a-time as each TreeRow mounts.
//   2. TreeRow.expandDir() — populates the cache after a live fetch so that
//      subsequent navigations into the same subtree are instant.
const dirCache = new Map<string, DirEntry[]>();

export function getCachedDir(source: string, prefix: string): DirEntry[] | undefined {
	return dirCache.get(`${source}:${prefix}`);
}

export function setCachedDir(source: string, prefix: string, entries: DirEntry[]): void {
	dirCache.set(`${source}:${prefix}`, entries);
}

/**
 * Pre-fetch every directory level that must be expanded to reach `filePath`,
 * storing results in the shared cache. Call fire-and-forget — no need to await.
 *
 * For a path like `src/lib/api.ts` this fetches `''`, `src/`, and `src/lib/`
 * in parallel. By the time each TreeRow's reactive expansion block runs, the
 * data is already in the cache and the expand is synchronous.
 *
 * Composite paths (`archive.zip::member`) are handled by stripping the `::…`
 * suffix — only outer filesystem directories are fetchable via listDir.
 */
export async function prefetchTreePath(source: string, filePath: string): Promise<void> {
	const outerPath = filePath.includes('::')
		? filePath.slice(0, filePath.indexOf('::'))
		: filePath;

	await Promise.allSettled(
		dirPrefixes(outerPath).map(async (prefix) => {
			const key = `${source}:${prefix}`;
			if (dirCache.has(key)) return;
			try {
				const resp = await listDir(source, prefix);
				dirCache.set(key, resp.entries);
			} catch {
				// Ignore — TreeRow will fall back to its own fetch.
			}
		})
	);
}

/**
 * Return all directory prefixes that must be open to reach filePath.
 * e.g. 'src/lib/api.ts' → ['', 'src/', 'src/lib/']
 */
function dirPrefixes(filePath: string): string[] {
	const parts = filePath.split('/');
	const prefixes: string[] = [''];
	let current = '';
	for (let i = 0; i < parts.length - 1; i++) {
		current += parts[i] + '/';
		prefixes.push(current);
	}
	return prefixes;
}

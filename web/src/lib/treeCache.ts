import { expandTreePath } from '$lib/api';
import type { DirEntry } from '$lib/api';

// Cache of directory listings, keyed by `${source}:${prefix}`.
//
// Populated two ways:
//   1. prefetchTreePath() — called fire-and-forget before navigation so all
//      intermediate levels are ready in a single request before TreeRow mounts.
//   2. TreeRow.expandDir() — uses the same expand endpoint on cache miss so
//      a single round-trip warms all ancestor levels simultaneously.
const dirCache = new Map<string, DirEntry[]>();

// In-flight expand promises, keyed by `${source}:${outerPath}`.
// Concurrent callers for the same path share one request instead of racing.
const inflight = new Map<string, Promise<void>>();

// Outer paths that have already been successfully expanded.
// Prevents redundant expand calls when archive member directories auto-expand
// serially (each level finishes before the next becomes visible, so the
// in-flight dedup alone doesn't catch them).
const expanded = new Set<string>();

export function getCachedDir(source: string, prefix: string): DirEntry[] | undefined {
	return dirCache.get(`${source}:${prefix}`);
}

export function setCachedDir(source: string, prefix: string, entries: DirEntry[]): void {
	dirCache.set(`${source}:${prefix}`, entries);
}

/**
 * Fetch all directory levels needed to reveal `filePath` in one request.
 * Concurrent calls for the same (source, filePath) share the in-flight promise.
 *
 * The server handles both outer filesystem directories and archive member
 * directories in a single response, so the full path is passed as-is.
 */
export function prefetchTreePath(source: string, filePath: string): Promise<void> {
	const key = `${source}:${filePath}`;
	if (expanded.has(key)) return Promise.resolve();
	const existing = inflight.get(key);
	if (existing) return existing;

	const promise = doExpand(source, filePath)
		.then(() => { expanded.add(key); })
		.finally(() => { inflight.delete(key); });
	inflight.set(key, promise);
	return promise;
}

async function doExpand(source: string, outerPath: string): Promise<void> {
	try {
		const resp = await expandTreePath(source, outerPath);
		for (const [prefix, entries] of Object.entries(resp.levels)) {
			if (!dirCache.has(`${source}:${prefix}`)) {
				dirCache.set(`${source}:${prefix}`, entries);
			}
		}
	} catch {
		// Ignore — TreeRow will fall back to its own listDir fetch.
	}
}

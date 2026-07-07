import type { SearchResult } from './api';

export interface MergePageResult {
	results: SearchResult[];
	newOffset: number;
}

/**
 * Merge a new page of search results into the existing list, deduplicating
 * by (source, path, archive_path, line_number) key.
 *
 * loadOffset must advance by incoming.length (the full server response size),
 * not by the number of fresh items added. If dedup removes items, using
 * fresh.length as the offset would re-request the same range and stall pagination.
 */
export function mergePage(
	existing: SearchResult[],
	incoming: SearchResult[],
	currentOffset: number
): MergePageResult {
	const seen = new Set(
		existing.map((r) => `${r.source}:${r.path}:${r.archive_path ?? ''}:${r.line_number}`)
	);
	const fresh = incoming.filter(
		(r) => !seen.has(`${r.source}:${r.path}:${r.archive_path ?? ''}:${r.line_number}`)
	);
	return {
		results: [...existing, ...fresh],
		newOffset: currentOffset + incoming.length
	};
}

/**
 * Compute the next raw-line offset for FileViewer's paged forward scroll.
 *
 * Must advance by the raw line range the server actually consumed
 * (`min(pageSize, totalLines - currentOffset)`), not by the number of lines
 * returned in the response. The server (`get_file_lines_paged`) silently
 * skips any raw line whose stored chunk lookup misses — e.g. blank lines
 * between TOML table entries in a Cargo.lock, which never got their own
 * content chunk — so a response can contain fewer lines than the raw range
 * it covered. Advancing by the response length under-counts in that case,
 * so the next page re-requests part of an already-loaded range and produces
 * duplicate line-number keys in CodeViewer's keyed `{#each}`.
 */
export function nextForwardOffset(currentOffset: number, pageSize: number, totalLines: number): number {
	return currentOffset + Math.min(pageSize, totalLines - currentOffset);
}

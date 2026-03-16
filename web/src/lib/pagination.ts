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

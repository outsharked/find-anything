import { getFile, type FileResponse } from './api';
import type { LinePage } from './virtualWindow';

/**
 * Normalize a `/api/v1/file` page into display-numbered lines.
 *
 * The server reports `line_offsets` in storage space where content starts at
 * line 2 (LINE_CONTENT_START) — subtract 1 to get 1-based display numbers.
 * When `line_offsets` is absent, lines are contiguous from the requested
 * fetch offset (0-based raw), so display numbers are `fetchOffset + i + 1`.
 */
export function normalizePage(data: Pick<FileResponse, 'lines' | 'line_offsets'>, fetchOffset: number): LinePage {
	const lineOffsets = data.line_offsets && data.line_offsets.length > 0
		? data.line_offsets.map((n) => n - 1)
		: data.lines.map((_, i) => fetchOffset + i + 1);
	return { lines: data.lines, lineOffsets };
}

export interface FetchedRange extends LinePage {
	/** The full server response, for callers that need metadata/total_lines. */
	data: FileResponse;
}

/** Fetch a raw-line range and normalize its offsets to display numbers. */
export async function fetchLineRange(
	source: string,
	path: string,
	archivePath: string | null,
	offset: number,
	limit: number
): Promise<FetchedRange> {
	const data = await getFile(source, path, archivePath ?? undefined, offset, limit);
	return { ...normalizePage(data, offset), data };
}

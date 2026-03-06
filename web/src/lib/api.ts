import { getToken } from './token';

// ── Types ────────────────────────────────────────────────────────────────────

export interface SourceInfo {
	name: string;
}

export interface ContextLine {
	line_number: number;
	content: string;
}

export interface SearchResult {
	source: string;
	path: string;
	archive_path: string | null;
	line_number: number;
	snippet: string;
	score: number;
	kind: string;
	mtime: number;
	size: number | null;
	context_lines: ContextLine[];
	/** Other paths with identical content (deduplication aliases). */
	aliases?: string[];
	/** Additional lines where query terms were found (document mode only). */
	extra_matches?: ContextLine[];
}

export interface SearchResponse {
	results: SearchResult[];
	total: number;
}

export interface FileResponse {
	lines: ContextLine[];
	file_kind: string;
	total_lines: number;
	mtime: number | null;
	size: number | null;
	indexing_error?: string;
}

export interface ContextResponse {
	start: number;
	/** Index within lines[] of the matched line; null if center fell in a gap. */
	match_index: number | null;
	lines: string[];
	kind: string;
}

export interface DirEntry {
	name: string;
	path: string;
	entry_type: 'dir' | 'file';
	kind?: string;
	size?: number;
	mtime?: number;
}

export interface TreeResponse {
	entries: DirEntry[];
}

// ── Auth ──────────────────────────────────────────────────────────────────────

export class AuthError extends Error {
	constructor() { super('Unauthorized'); }
}

function authHeaders(extra?: Record<string, string>): Record<string, string> {
	return { Authorization: `Bearer ${getToken()}`, ...extra };
}

async function apiFetch(url: string, init?: RequestInit): Promise<Response> {
	const resp = await fetch(url, {
		...init,
		headers: { ...authHeaders(), ...(init?.headers as Record<string, string> | undefined) }
	});
	if (resp.status === 401) throw new AuthError();
	return resp;
}

/**
 * Sets the find_session cookie so browser-native requests (e.g. <img src>)
 * can be authenticated without custom headers. Best-effort: header auth still
 * works if this call fails.
 */
export async function activateSession(): Promise<void> {
	const token = getToken();
	if (!token) return;
	await fetch('/api/v1/auth/session', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json', ...authHeaders() },
		body: JSON.stringify({ token }),
	}).catch(() => {});
}

/**
 * Clears the find_session cookie on the server side.
 */
export async function clearSession(): Promise<void> {
	await fetch('/api/v1/auth/session', { method: 'DELETE' }).catch(() => {});
}

// ── API calls ─────────────────────────────────────────────────────────────────

export async function listSources(): Promise<SourceInfo[]> {
	const resp = await apiFetch('/api/v1/sources');
	if (!resp.ok) throw new Error(`listSources: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

export interface SearchParams {
	q: string;
	mode?: string;
	sources?: string[];
	limit?: number;
	offset?: number;
	/** Unix timestamp seconds (inclusive lower bound for file mtime). */
	dateFrom?: number;
	/** Unix timestamp seconds (inclusive upper bound for file mtime). */
	dateTo?: number;
}

export async function search(params: SearchParams): Promise<SearchResponse> {
	const url = new URL('/api/v1/search', location.origin);
	url.searchParams.set('q', params.q);
	if (params.mode) url.searchParams.set('mode', params.mode);
	if (params.sources && params.sources.length > 0) {
		params.sources.forEach((s) => url.searchParams.append('source', s));
	}
	if (params.limit != null) url.searchParams.set('limit', String(params.limit));
	if (params.offset != null) url.searchParams.set('offset', String(params.offset));
	if (params.dateFrom != null) url.searchParams.set('date_from', String(params.dateFrom));
	if (params.dateTo != null) url.searchParams.set('date_to', String(params.dateTo));

	const resp = await apiFetch(url.toString());
	if (!resp.ok) {
		const errorText = await resp.text().catch(() => resp.statusText);
		throw new Error(`Search failed: ${errorText || resp.statusText}`);
	}
	return resp.json();
}

export async function getFile(
	source: string,
	path: string,
	archivePath?: string
): Promise<FileResponse> {
	const url = new URL('/api/v1/file', location.origin);
	url.searchParams.set('source', source);
	url.searchParams.set('path', path);
	if (archivePath) url.searchParams.set('archive_path', archivePath);

	const resp = await apiFetch(url.toString());
	if (!resp.ok) throw new Error(`getFile: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

export async function listFiles(source: string): Promise<FileRecord[]> {
	const url = new URL('/api/v1/files', location.origin);
	url.searchParams.set('source', source);
	const resp = await apiFetch(url.toString());
	if (!resp.ok) throw new Error(`listFiles: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

export interface FileRecord {
	path: string;
	mtime: number;
	kind: string;
}

export async function listDir(source: string, prefix = ''): Promise<TreeResponse> {
	const url = new URL('/api/v1/tree', location.origin);
	url.searchParams.set('source', source);
	if (prefix) url.searchParams.set('prefix', prefix);

	const resp = await apiFetch(url.toString());
	if (!resp.ok) throw new Error(`listDir: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

/** List the inner members of an archive file by using the "::" prefix convention. */
export async function listArchiveMembers(source: string, archivePath: string): Promise<TreeResponse> {
	return listDir(source, archivePath + '::');
}

export interface ContextBatchItem {
	source: string;
	path: string;
	archive_path?: string | null;
	line: number;
	window?: number;
}

export interface ContextBatchResult {
	source: string;
	path: string;
	line: number;
	start: number;
	match_index: number | null;
	lines: string[];
	kind: string;
}

export interface ContextBatchResponse {
	results: ContextBatchResult[];
}

export async function contextBatch(requests: ContextBatchItem[]): Promise<ContextBatchResponse> {
	const resp = await apiFetch('/api/v1/context-batch', {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify({ requests })
	});
	if (!resp.ok) throw new Error(`contextBatch: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

export async function getContext(
	source: string,
	path: string,
	line: number,
	window = 5,
	archivePath?: string
): Promise<ContextResponse> {
	const url = new URL('/api/v1/context', location.origin);
	url.searchParams.set('source', source);
	url.searchParams.set('path', path);
	url.searchParams.set('line', String(line));
	url.searchParams.set('window', String(window));
	if (archivePath) url.searchParams.set('archive_path', archivePath);

	const resp = await apiFetch(url.toString());
	if (!resp.ok) throw new Error(`getContext: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

export interface AppSettings {
	context_window: number;
	version: string;
	git_hash: string;
}

export async function getSettings(): Promise<AppSettings> {
	const resp = await apiFetch('/api/v1/settings');
	if (!resp.ok) throw new Error(`getSettings: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

// ── Stats ─────────────────────────────────────────────────────────────────────

export interface KindStats {
	count: number;
	size: number;
	avg_extract_ms: number | null;
}

export interface ScanHistoryPoint {
	scanned_at: number;
	total_files: number;
	total_size: number;
}

export interface ExtStat {
	ext: string;
	count: number;
	size: number;
}

export interface SourceStats {
	name: string;
	last_scan: number | null;
	total_files: number;
	total_size: number;
	by_kind: Record<string, KindStats>;
	by_ext: ExtStat[];
	history: ScanHistoryPoint[];
	indexing_error_count: number;
}

export type WorkerStatus =
	| { state: 'idle' }
	| { state: 'processing'; source: string; file: string };

export interface StatsResponse {
	sources: SourceStats[];
	inbox_pending: number;
	failed_requests: number;
	total_archives: number;
	db_size_bytes: number;
	archive_size_bytes: number;
	worker_status: WorkerStatus;
}

export async function getStats(): Promise<StatsResponse> {
	const resp = await apiFetch('/api/v1/stats');
	if (!resp.ok) throw new Error(`getStats: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

// ── Indexing errors ───────────────────────────────────────────────────────────

export interface IndexingError {
	path: string;
	error: string;
	first_seen: number;
	last_seen: number;
	count: number;
}

export interface ErrorsResponse {
	errors: IndexingError[];
	total: number;
}

export async function getErrors(
	source: string,
	limit = 200,
	offset = 0,
): Promise<ErrorsResponse> {
	const url = new URL('/api/v1/errors', location.origin);
	url.searchParams.set('source', source);
	url.searchParams.set('limit', String(limit));
	url.searchParams.set('offset', String(offset));
	const resp = await apiFetch(url.toString());
	if (!resp.ok) throw new Error(`getErrors: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

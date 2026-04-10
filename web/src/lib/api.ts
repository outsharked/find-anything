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
	/** Other paths with identical content. */
	duplicate_paths?: string[];
	/** Additional lines where query terms were found (document mode only). */
	extra_matches?: ContextLine[];
	/** True when this file had more matching lines than the display cap (document mode only). */
	hits_truncated?: boolean;
}

export interface SearchResponse {
	results: SearchResult[];
	total: number;
	/** True when the result set was capped; display "N+" instead of "N". */
	capped: boolean;
}

export interface FileResponse {
	/** Content lines in order; display line number = index + 1 unless line_offsets is present. */
	lines: string[];
	/** Actual line numbers when not a contiguous 1-based sequence (e.g. sparse PDFs). */
	line_offsets?: number[];
	/** Path/metadata entries (line_number < content_line_start). */
	metadata: string[];
	file_kind: string;
	total_lines: number;
	mtime: number | null;
	size: number | null;
	indexing_error?: string;
	/** True when content is indexed but not yet written to the archive by the background worker. */
	content_unavailable?: boolean;
	/** Other paths with identical content. */
	duplicate_paths?: string[];
}

export interface ContextResponse {
	start: number;
	/** Index within lines[] of the matched line; null if center fell in a gap. */
	match_index: number | null;
	/** Each line carries its own line_number — use line.line_number, not start + index. */
	lines: ContextLine[];
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

export interface TreeExpandResponse {
	levels: Record<string, DirEntry[]>;
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
	/** Allowlist of file kinds (e.g. "pdf", "image"). Empty/omitted = any kind. */
	kinds?: string[];
	/** When true, matching is case-sensitive. Default: false (case-insensitive). */
	caseSensitive?: boolean;
	/** Restrict results to files whose path starts with this prefix (no leading slash). */
	pathPrefix?: string;
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
	if (params.kinds && params.kinds.length > 0) {
		params.kinds.forEach((k) => url.searchParams.append('kind', k));
	}
	if (params.caseSensitive) url.searchParams.set('case_sensitive', '1');
	if (params.pathPrefix) url.searchParams.set('path_prefix', params.pathPrefix);

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
	archivePath?: string,
	offset?: number,
	limit?: number
): Promise<FileResponse> {
	const url = new URL('/api/v1/file', location.origin);
	url.searchParams.set('source', source);
	url.searchParams.set('path', path);
	if (archivePath) url.searchParams.set('archive_path', archivePath);
	if (offset != null) url.searchParams.set('offset', String(offset));
	if (limit != null) url.searchParams.set('limit', String(limit));

	const resp = await apiFetch(url.toString());
	if (!resp.ok) throw new Error(`getFile: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

export async function listFiles(source: string, q?: string, limit = 50): Promise<FileRecord[]> {
	const url = new URL('/api/v1/files', location.origin);
	url.searchParams.set('source', source);
	if (q !== undefined) {
		url.searchParams.set('q', q);
		url.searchParams.set('limit', String(limit));
	}
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

/** Fetch all directory levels needed to reveal `path` in one request. */
export async function expandTreePath(source: string, path: string): Promise<TreeExpandResponse> {
	const url = new URL('/api/v1/tree/expand', location.origin);
	url.searchParams.set('source', source);
	url.searchParams.set('path', path);

	const resp = await apiFetch(url.toString());
	if (!resp.ok) throw new Error(`expandTreePath: ${resp.status} ${resp.statusText}`);
	return resp.json();
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
	/** Each line carries its own line_number — use line.line_number, not start + index. */
	lines: ContextLine[];
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
	/** Maximum markdown file size (KB) the UI will render as formatted HTML. */
	max_markdown_render_kb: number;
	/** Maximum content lines per /api/v1/file request. 0 = no limit. */
	file_view_page_size: number;
	/** Number of spaces a tab character occupies in the file viewer. Defaults to 4. */
	tab_width?: number;
	/** Public base URL of the server (e.g. `https://find.example.com`). Used as the origin for share links. */
	public_url?: string;
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
	content_file_count: number;
	db_size_bytes: number;
	content_size_bytes: number;
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

// ── Share links ───────────────────────────────────────────────────────────────

export interface CreateLinkResponse {
	code: string;
	/** Relative URL for the direct view page, e.g. `/v/aB3mZx`. */
	url: string;
	/** Unix timestamp (seconds) when this link expires. */
	expires_at: number;
}

export interface ResolveLinkResponse {
	source: string;
	path: string;
	archive_path: string | null;
	kind: string;
	filename: string;
	mtime: number;
	expires_at: number;
}

/** Create a share link for a file. Requires authentication.
 * @param expiresInSecs TTL in seconds. Omit = server default. 0 = never expires. */
export async function createLink(
	source: string,
	path: string,
	archivePath?: string | null,
	expiresInSecs?: number
): Promise<CreateLinkResponse> {
	const body: Record<string, unknown> = { source, path, archive_path: archivePath ?? null };
	if (expiresInSecs !== undefined) body.expires_in_secs = expiresInSecs;
	const resp = await apiFetch('/api/v1/links', {
		method: 'POST',
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!resp.ok) throw new Error(`createLink: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

/** Resolve a share link code. No authentication required. Returns null if not found, 'expired' if expired. */
export async function resolveLink(
	code: string
): Promise<ResolveLinkResponse | 'expired' | null> {
	const resp = await fetch(`/api/v1/links/${encodeURIComponent(code)}`);
	if (resp.status === 404) return null;
	if (resp.status === 410) return 'expired';
	if (!resp.ok) throw new Error(`resolveLink: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

// ── Admin inbox ───────────────────────────────────────────────────────────────

export interface InboxItem {
	filename: string;
	size_bytes: number;
	age_secs: number;
}

export interface InboxStatusResponse {
	pending: InboxItem[];
	failed: InboxItem[];
}

export async function getInboxStatus(): Promise<InboxStatusResponse> {
	const resp = await apiFetch('/api/v1/admin/inbox');
	if (!resp.ok) throw new Error(`getInboxStatus: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

export async function retryFailedInbox(): Promise<{ retried: number }> {
	const resp = await apiFetch('/api/v1/admin/inbox/retry', { method: 'POST' });
	if (!resp.ok) throw new Error(`retryFailedInbox: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

// ── Self-update ───────────────────────────────────────────────────────────────

export interface UpdateCheckResponse {
	current: string;
	latest: string;
	update_available: boolean;
	restart_supported: boolean;
	restart_unsupported_reason?: string;
}

export interface UpdateApplyResponse {
	ok: boolean;
	message: string;
}

export async function getUpdateCheck(): Promise<UpdateCheckResponse> {
	const resp = await apiFetch('/api/v1/admin/update/check');
	if (!resp.ok) throw new Error(`getUpdateCheck: ${resp.status} ${resp.statusText}`);
	return resp.json();
}

export async function applyUpdate(): Promise<UpdateApplyResponse> {
	const resp = await apiFetch('/api/v1/admin/update/apply', { method: 'POST' });
	if (!resp.ok) {
		const body = await resp.json().catch(() => ({ message: resp.statusText }));
		throw new Error(body.message ?? resp.statusText);
	}
	return resp.json();
}

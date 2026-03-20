/**
 * Constructs a `findanything://open?path=...` URL for the "Open in Explorer"
 * feature.  The caller passes the configured local root for the source and the
 * server-relative file path; this function joins them with the right separator
 * and URL-encodes the result.
 *
 * Separator detection: if the root contains a backslash it is treated as a
 * Windows path (`\`), otherwise forward slash is used.
 *
 * For archive members pass only the outer file path (not the composite
 * `outer::member` path) — the file manager can select the archive file but
 * cannot navigate into a virtual member path.
 */
export function buildExplorerUrl(sourceRoot: string, filePath: string): string {
	const sep = sourceRoot.includes('\\') ? '\\' : '/';
	const rel = filePath.split('/').join(sep);
	const full = sourceRoot.replace(/[\\/]+$/, '') + sep + rel;
	return 'findanything://open?path=' + encodeURIComponent(full);
}

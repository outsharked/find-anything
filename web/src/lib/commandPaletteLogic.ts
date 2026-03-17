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

/**
 * Parse the "go to line" dialog's input into a 1-based line number.
 *
 * Accepts a plain number ("5000") as well as forms a user might paste in
 * from a hash link or another tool ("L5000", "#L5000"). Returns null for
 * anything that isn't a positive integer.
 */
export function parseGoToLineInput(raw: string): number | null {
	const trimmed = raw.trim();
	const body = trimmed.startsWith('#L')
		? trimmed.slice(2)
		: trimmed.startsWith('L') || trimmed.startsWith('l')
			? trimmed.slice(1)
			: trimmed;
	if (!/^\d+$/.test(body)) return null;
	const n = parseInt(body, 10);
	return n > 0 ? n : null;
}

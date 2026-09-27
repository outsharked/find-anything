/**
 * Pure helpers for the Admin panel's access-token / invite management UI
 * (plan 094). Kept separate from AdminPanel.svelte so the parsing and
 * validation rules are unit-testable.
 */

import type { Scope } from './api';

export const SCOPE_OPTIONS: { value: Scope; label: string; hint: string }[] = [
	{ value: 'read', label: 'Read', hint: 'Search, browse and view files' },
	{ value: 'update-index', label: 'Update index', hint: 'Read, plus scanning/uploading' },
	{ value: 'admin', label: 'Admin', hint: 'Everything, including this panel' }
];

/** `null` when the name is acceptable; otherwise a message to show the user. */
export function validateInviteName(name: string): string | null {
	const trimmed = name.trim();
	if (!trimmed) return 'Name is required.';
	if ([...trimmed].some((c) => c.charCodeAt(0) < 32)) return 'Name cannot contain control characters.';
	return null;
}

/**
 * Parse a duration string like `90`, `30s`, `15m`, `2h`, `7d`, `2w` into
 * seconds. Mirrors `find-admin`'s `parse_duration_secs` so the web form
 * accepts the same syntax. Returns `undefined` for blank input (caller should
 * fall back to the server default), or `null` if the input is invalid.
 */
export function parseDurationSecs(input: string): number | undefined | null {
	const s = input.trim();
	if (!s) return undefined;
	const unit = s[s.length - 1];
	const multipliers: Record<string, number> = { s: 1, m: 60, h: 3600, d: 86_400, w: 604_800 };
	let numPart: string;
	let mult: number;
	if (unit in multipliers) {
		numPart = s.slice(0, -1).trim();
		mult = multipliers[unit];
	} else if (/^[0-9]+$/.test(s)) {
		numPart = s;
		mult = 1;
	} else {
		return null;
	}
	if (!/^[0-9]+$/.test(numPart)) return null;
	const n = Number(numPart);
	if (!Number.isFinite(n) || n <= 0) return null;
	const secs = n * mult;
	return Number.isSafeInteger(secs) ? secs : null;
}

/** `"never"` for `null`/`undefined`, otherwise a locale-formatted date/time. */
export function formatTimestamp(ts: number | null | undefined): string {
	if (ts === null || ts === undefined) return 'never';
	return new Date(ts * 1000).toLocaleString();
}

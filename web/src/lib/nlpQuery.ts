import * as chrono from 'chrono-node';

// ── Types ─────────────────────────────────────────────────────────────────────

export interface NlpResult {
	/** Cleaned query to send to the API (date phrase + stop words removed). */
	query: string;
	/** Unix timestamp seconds (inclusive lower bound). */
	dateFrom?: number;
	/** Unix timestamp seconds (inclusive upper bound). */
	dateTo?: number;
	/** Human-readable label for the chip, e.g. "Mar 4 – Mar 6". */
	dateLabel?: string;
	/** The raw phrase that was detected, for the conflict tooltip. */
	detectedPhrase?: string;
}

// ── Constants ─────────────────────────────────────────────────────────────────

const STOP_WORDS = new Set(['a', 'an', 'the', 'and']);

// Words that can precede a chrono date expression and belong to the date phrase.
// Matches one or more consecutive date-linking words (e.g. "in the", "between", "since last").
const DATE_PREFIX_RE = /\b((in|on|since|before|after|between|from|until|during|last|next|past|over|the|within)\s+)+$/i;

// Words that indicate the result is an upper bound only (date_to, no date_from).
const UPPER_BOUND_RE = /\b(before|until|by)\s*$/i;

// Date phrases that refer to a single complete past day (not an open range to now).
const WHOLE_DAY_RE = /^\s*(yesterday|last\s+(monday|tuesday|wednesday|thursday|friday|saturday|sunday))\s*$/i;

// "last week" / "last weekend" require calendar-period computation.
const LAST_WEEK_RE    = /^\s*last\s+week\s*$/i;
const LAST_WEEKEND_RE = /^\s*last\s+weekend\s*$/i;

// Prefix words that indicate a rolling window ("in the last X", "within the last X").
// When these precede a period expression, use "now" as the upper bound rather than end-of-period.
const ROLLING_PREFIX_RE = /\b(in|within)\b/i;

// Words that indicate an open-ended lower bound (date_from → now).
const LOWER_BOUND_RE = /\b(since|after)\s*$/i;

// ── Helpers ───────────────────────────────────────────────────────────────────

const DATE_FMT = new Intl.DateTimeFormat('en', { month: 'short', day: 'numeric' });
const DATE_YEAR_FMT = new Intl.DateTimeFormat('en', { month: 'short', day: 'numeric', year: 'numeric' });

function toUnix(d: Date): number {
	return Math.floor(d.getTime() / 1000);
}

function endOfDay(d: Date): Date {
	const r = new Date(d);
	r.setUTCHours(23, 59, 59, 999);
	return r;
}

/** Monday–Sunday of the calendar week before `now` (UTC). */
function prevCalendarWeek(now: Date): [Date, Date] {
	const dow = now.getUTCDay(); // 0=Sun … 6=Sat
	const daysSinceMon = (dow + 6) % 7;
	const mon = new Date(now);
	mon.setUTCDate(now.getUTCDate() - daysSinceMon - 7);
	mon.setUTCHours(0, 0, 0, 0);
	const sun = new Date(mon);
	sun.setUTCDate(mon.getUTCDate() + 6);
	sun.setUTCHours(23, 59, 59, 999);
	return [mon, sun];
}

/** Saturday + Sunday of the most recently completed weekend before `now` (UTC). */
function prevWeekend(now: Date): [Date, Date] {
	const dow = now.getUTCDay(); // 0=Sun … 6=Sat
	// Days since most recent Saturday (if today is Sat, go back to last Sat)
	const daysSinceSat = (dow - 6 + 7) % 7 || 7;
	const sat = new Date(now);
	sat.setUTCDate(now.getUTCDate() - daysSinceSat);
	sat.setUTCHours(0, 0, 0, 0);
	const sun = new Date(sat);
	sun.setUTCDate(sat.getUTCDate() + 1);
	sun.setUTCHours(23, 59, 59, 999);
	return [sat, sun];
}

/**
 * Normalize the lower bound to the start of the implied period when the day is not certain.
 * When `rolling` is true (e.g. "in the last month"), keep chrono's exact start point.
 * When `rolling` is false (e.g. "last month"), snap to 1st of month / Jan 1 of year.
 */
function lowerBound(result: chrono.ParsedResult, rolling: boolean): Date {
	const start = result.start.date();
	if (!result.start.isCertain('day') && !rolling) {
		const d = new Date(start);
		if (result.start.isCertain('month')) {
			d.setUTCDate(1);
			d.setUTCHours(0, 0, 0, 0);
		} else if (result.start.isCertain('year')) {
			d.setUTCMonth(0, 1);
			d.setUTCHours(0, 0, 0, 0);
		}
		return d;
	}
	return start;
}

/**
 * Compute the upper bound date for a parsed result.
 * When `rolling` is true (e.g. "in the last year"), use now.
 * When `rolling` is false (e.g. "last year/month"), use end of the calendar period.
 */
function upperBound(result: chrono.ParsedResult, now: Date, rolling: boolean): Date {
	if (result.end) return result.end.date();
	if (!result.start.isCertain('day')) {
		if (rolling) return now;
		if (result.start.isCertain('month')) {
			// End of that calendar month.
			const d = new Date(result.start.date());
			d.setUTCMonth(d.getUTCMonth() + 1, 0);
			d.setUTCHours(23, 59, 59, 999);
			return d;
		}
		// End of that calendar year.
		const d = new Date(result.start.date());
		d.setUTCMonth(11, 31);
		d.setUTCHours(23, 59, 59, 999);
		return d;
	}
	if (WHOLE_DAY_RE.test(result.text)) {
		return endOfDay(result.start.date());
	}
	return now;
}

function sameYear(a: Date, b: Date): boolean {
	return a.getFullYear() === b.getFullYear();
}

function formatDate(d: Date, now: Date): string {
	return sameYear(d, now) ? DATE_FMT.format(d) : DATE_YEAR_FMT.format(d);
}

function makeLabel(from: number | undefined, to: number | undefined, now: Date): string {
	if (from != null && to != null) {
		const f = new Date(from * 1000);
		const t = new Date(to * 1000);
		// Collapse "Mar 4 – Mar 4" to just "Mar 4"
		if (f.toDateString() === t.toDateString()) return formatDate(f, now);
		return `${formatDate(f, now)} – ${formatDate(t, now)}`;
	}
	if (from != null) return `after ${formatDate(new Date(from * 1000), now)}`;
	if (to != null) return `before ${formatDate(new Date(to * 1000), now)}`;
	return '';
}

// ── Main export ───────────────────────────────────────────────────────────────

/**
 * Parse natural-language date phrases and strip stop words from a search query.
 *
 * - Regex mode: returned unchanged.
 * - Exact mode: date extraction only (no stop word stripping).
 * - Text/document mode: both transforms applied.
 *
 * Quoted segments ("like this") are always preserved verbatim.
 */
export function parseNlpQuery(raw: string, mode: string): NlpResult {
	if (mode === 'regex') return { query: raw };

	const now = new Date();

	// 1. Extract quoted segments, replace with null-byte placeholders.
	const quoted: string[] = [];
	const unquoted = raw.replace(/"[^"]*"/g, (match) => {
		quoted.push(match);
		return `\x00${quoted.length - 1}\x00`;
	});

	// 2. Try date extraction.
	const parsed = chrono.casual.parse(unquoted, now);

	let dateFrom: number | undefined;
	let dateTo: number | undefined;
	let detectedPhrase: string | undefined;
	let removeSpan: [number, number] | null = null;

	if (parsed.length === 1) {
		const result = parsed[0];
		const before = unquoted.slice(0, result.index);
		const prefixMatch = before.match(DATE_PREFIX_RE);
		const spanStart = prefixMatch ? result.index - prefixMatch[0].length : result.index;
		const spanEnd = result.index + result.text.length;

		// "in the last X" / "within the last X" → rolling window to now.
		// Both conditions must hold: a rolling prefix ("in"/"within") AND the date phrase
		// itself is relative ("last year", "last 2 days"). Named periods like "in January"
		// have the prefix but no relative word in the phrase → not rolling.
		const rolling = ROLLING_PREFIX_RE.test(before) && /\b(last|past|ago|previous)\b/i.test(result.text);

		if (UPPER_BOUND_RE.test(before)) {
			// "before [date]" → upper bound only
			dateTo = toUnix(upperBound(result, now, true));
		} else if (LOWER_BOUND_RE.test(before)) {
			// "since [date]" / "after [date]" → lower bound to now
			dateFrom = toUnix(result.start.date());
			dateTo = toUnix(now);
		} else if (!rolling && LAST_WEEK_RE.test(result.text)) {
			// "last week" → Mon–Sun of the previous calendar week.
			const [mon, sun] = prevCalendarWeek(now);
			dateFrom = toUnix(mon);
			dateTo = toUnix(sun);
		} else if (!rolling && LAST_WEEKEND_RE.test(result.text)) {
			// "last weekend" → Sat+Sun of the most recent past weekend.
			const [sat, sun] = prevWeekend(now);
			dateFrom = toUnix(sat);
			dateTo = toUnix(sun);
		} else {
			// General: "last month", "last year", "yesterday", "in January", "last two days"
			dateFrom = toUnix(lowerBound(result, rolling));
			dateTo = toUnix(upperBound(result, now, rolling));
		}

		detectedPhrase = unquoted.slice(spanStart, spanEnd).trim();
		removeSpan = [spanStart, spanEnd];
	} else if (parsed.length >= 2) {
		// "between X and Y" / "X to Y" / "from X to Y"
		const first = parsed[0];
		const second = parsed[parsed.length - 1];
		const connector = unquoted
			.slice(first.index + first.text.length, second.index)
			.trim()
			.toLowerCase();

		if (connector === 'and' || connector === 'to' || connector === '-') {
			const before = unquoted.slice(0, first.index);
			const prefixMatch = before.match(/\b(between|from)\s+$/i);
			const spanStart = prefixMatch ? first.index - prefixMatch[0].length : first.index;
			const spanEnd = second.index + second.text.length;

			dateFrom = toUnix(first.start.date());
			dateTo = toUnix(upperBound(second, now, false));

			detectedPhrase = unquoted.slice(spanStart, spanEnd).trim();
			removeSpan = [spanStart, spanEnd];
		}
	}

	// Guard: if the computed range is backwards (e.g. chrono picked a future date for
	// "since monday"), discard the extraction so the raw query goes through unchanged.
	if (dateFrom != null && dateTo != null && dateFrom > dateTo) {
		dateFrom = undefined;
		dateTo = undefined;
		detectedPhrase = undefined;
		removeSpan = null;
	}

	// 3. Remove the detected date phrase from the unquoted text.
	let cleaned = unquoted;
	if (removeSpan) {
		cleaned = (unquoted.slice(0, removeSpan[0]) + ' ' + unquoted.slice(removeSpan[1]))
			.replace(/\s+/g, ' ')
			.trim();
	}

	// 4. Strip stop words (text/document modes only, not exact).
	if (mode !== 'exact') {
		cleaned = cleaned
			.split(/\s+/)
			.filter((token) => {
				if (!token || token.includes('\x00')) return true; // keep placeholders
				return !STOP_WORDS.has(token.toLowerCase());
			})
			.join(' ')
			.trim();
	}

	// 5. Restore quoted segments.
	const finalQuery = cleaned
		.replace(/\x00(\d+)\x00/g, (_, i) => quoted[Number(i)])
		.trim();

	// 6. Safety: if the cleaned query is empty, revert to the raw query
	//    (don't send a blank query and silently filter only by date).
	if (!finalQuery) {
		return { query: raw };
	}

	const dateLabel = makeLabel(dateFrom, dateTo, now);

	return { query: finalQuery, dateFrom, dateTo, dateLabel: dateLabel || undefined, detectedPhrase };
}

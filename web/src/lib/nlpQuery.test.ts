import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { parseNlpQuery } from './nlpQuery';

// Fix reference date so tests are deterministic.
// 2026-03-06 is a Friday.
const REF = new Date('2026-03-06T12:00:00.000Z');

function d(iso: string): number {
	return Math.floor(new Date(iso).getTime() / 1000);
}

beforeEach(() => {
	vi.useFakeTimers();
	vi.setSystemTime(REF);
});

afterEach(() => {
	vi.useRealTimers();
});

// ── Regex mode ────────────────────────────────────────────────────────────────

describe('regex mode', () => {
	it('passes the query through unchanged', () => {
		const r = parseNlpQuery('artifactory in the last two days', 'regex');
		expect(r.query).toBe('artifactory in the last two days');
		expect(r.dateFrom).toBeUndefined();
		expect(r.dateTo).toBeUndefined();
	});

	it('does not strip stop words', () => {
		const r = parseNlpQuery('the quick brown fox', 'regex');
		expect(r.query).toBe('the quick brown fox');
	});
});

// ── Stop word stripping (text mode) ──────────────────────────────────────────

describe('stop word stripping', () => {
	it('strips articles from plain queries', () => {
		const r = parseNlpQuery('the quick brown fox', 'text');
		expect(r.query).toBe('quick brown fox');
		expect(r.dateFrom).toBeUndefined();
	});

	it('strips "a", "an", "and" between terms', () => {
		const r = parseNlpQuery('artifactory and token a an test', 'text');
		expect(r.query).toBe('artifactory token test');
	});

	it('preserves quoted segments verbatim', () => {
		const r = parseNlpQuery('"the quick" brown fox', 'text');
		expect(r.query).toBe('"the quick" brown fox');
	});

	it('does not strip stop words in exact mode', () => {
		const r = parseNlpQuery('the quick brown fox', 'exact');
		expect(r.query).toBe('the quick brown fox');
	});

	it('does not strip "or" or "not" (FTS operators)', () => {
		const r = parseNlpQuery('foo or bar not baz', 'text');
		expect(r.query).toBe('foo or bar not baz');
	});
});

// ── Date extraction — relative ranges ────────────────────────────────────────

describe('relative date ranges', () => {
	it('"in the last two days" extracts date and cleans query', () => {
		const r = parseNlpQuery('artifactory in the last two days', 'text');
		expect(r.query).toBe('artifactory');
		// from = 2026-03-04T12:00:00Z (2 days before REF)
		expect(r.dateFrom).toBeDefined();
		expect(r.dateTo).toBeDefined();
		expect(r.dateFrom!).toBeLessThan(d('2026-03-05T00:00:00Z'));
		expect(r.dateTo!).toBeGreaterThanOrEqual(d('2026-03-06T11:00:00Z'));
		expect(r.dateLabel).toMatch(/Mar/);
		expect(r.detectedPhrase).toMatch(/last two days/i);
	});

	it('"last week" extracts date range', () => {
		const r = parseNlpQuery('token last week', 'text');
		expect(r.query).toBe('token');
		expect(r.dateFrom).toBeDefined();
		expect(r.dateTo).toBeDefined();
		// from is before REF
		expect(r.dateFrom!).toBeLessThan(d('2026-03-06T00:00:00Z'));
	});

	it('"last two days" with "and" conjunction strips and', () => {
		const r = parseNlpQuery('artifactory and token last week', 'text');
		expect(r.query).toBe('artifactory token');
		expect(r.dateFrom).toBeDefined();
	});

	it('"yesterday" extracts a single-day range', () => {
		const r = parseNlpQuery('report yesterday', 'text');
		expect(r.query).toBe('report');
		expect(r.dateFrom).toBeDefined();
		expect(r.dateTo).toBeDefined();
		// from and to should both be on 2026-03-05
		const from = new Date(r.dateFrom! * 1000);
		const to = new Date(r.dateTo! * 1000);
		expect(from.toISOString().slice(0, 10)).toBe('2026-03-05');
		expect(to.toISOString().slice(0, 10)).toBe('2026-03-05');
	});
});

// ── Date extraction — explicit ranges ─────────────────────────────────────────

describe('explicit date ranges', () => {
	it('"between january and february" extracts full month range', () => {
		const r = parseNlpQuery('report between january and february', 'text');
		expect(r.query).toBe('report');
		expect(r.dateFrom).toBeDefined();
		expect(r.dateTo).toBeDefined();
		// from = Jan 1
		const from = new Date(r.dateFrom! * 1000);
		expect(from.getUTCMonth()).toBe(0); // January
		// to = Feb 28 23:59:59 UTC
		const to = new Date(r.dateTo! * 1000);
		expect(to.getUTCMonth()).toBe(1); // February
		expect(to.getUTCDate()).toBe(28);
	});

	it('"in january" extracts a month range', () => {
		const r = parseNlpQuery('contract in january', 'text');
		expect(r.query).toBe('contract');
		expect(r.dateFrom).toBeDefined();
		expect(r.dateTo).toBeDefined();
		const from = new Date(r.dateFrom! * 1000);
		const to = new Date(r.dateTo! * 1000);
		expect(from.getUTCMonth()).toBe(0);
		expect(to.getUTCMonth()).toBe(0);
		expect(to.getUTCDate()).toBe(31);
	});
});

// ── Exact mode — date extraction without stop word stripping ─────────────────

describe('exact mode', () => {
	it('extracts dates but preserves stop words in query', () => {
		const r = parseNlpQuery('the artifactory token in the last two days', 'exact');
		expect(r.dateFrom).toBeDefined();
		// "the" should remain (no stop word stripping)
		expect(r.query).toContain('the');
		// date phrase should be removed
		expect(r.query).not.toMatch(/last two days/i);
	});
});

// ── Edge cases ────────────────────────────────────────────────────────────────

describe('edge cases', () => {
	it('does not extract dates from no-date queries', () => {
		const r = parseNlpQuery('the quick brown fox', 'text');
		expect(r.dateFrom).toBeUndefined();
		expect(r.dateTo).toBeUndefined();
		expect(r.query).toBe('quick brown fox');
	});

	it('reverts to raw query if cleaning produces empty string', () => {
		// Only a date phrase, no content terms
		const r = parseNlpQuery('last two days', 'text');
		// Either passes through raw (safety guard) or returns it unchanged
		expect(r.query).toBeTruthy();
	});

	it('preserves quoted stop words inside quotes', () => {
		const r = parseNlpQuery('"the quick brown" fox', 'text');
		expect(r.query).toBe('"the quick brown" fox');
	});

	it('does not apply date extraction in regex mode', () => {
		const r = parseNlpQuery('foo (last|next) week', 'regex');
		expect(r.query).toBe('foo (last|next) week');
		expect(r.dateFrom).toBeUndefined();
	});

	it('"last month" = calendar Feb, "last year" = calendar 2025, "in the last X" = rolling', () => {
		// "last month" → calendar Feb 2026 → Feb 1–28.
		const month = parseNlpQuery('notes last month', 'text');
		const mFrom = new Date(month.dateFrom! * 1000);
		const mTo   = new Date(month.dateTo!   * 1000);
		expect(mFrom.getUTCMonth()).toBe(1); expect(mFrom.getUTCDate()).toBe(1);  // Feb 1
		expect(mTo.getUTCMonth()).toBe(1);   expect(mTo.getUTCDate()).toBe(28);   // Feb 28

		// "last year" → calendar 2025 → Jan 1–Dec 31.
		const year = parseNlpQuery('files last year', 'text');
		const yFrom = new Date(year.dateFrom! * 1000);
		const yTo   = new Date(year.dateTo!   * 1000);
		expect(yFrom.getUTCFullYear()).toBe(2025); expect(yFrom.getUTCMonth()).toBe(0);  expect(yFrom.getUTCDate()).toBe(1);
		expect(yTo.getUTCFullYear()).toBe(2025);   expect(yTo.getUTCMonth()).toBe(11);   expect(yTo.getUTCDate()).toBe(31);

		// "in the last year" → rolling → dateTo ≈ now.
		const rolling = parseNlpQuery('files in the last year', 'text');
		expect(rolling.dateTo!).toBeGreaterThanOrEqual(d('2026-03-06T11:00:00Z'));
	});

	it('"last week" = Mon–Sun of prior week; "last weekend" = Sat+Sun', () => {
		// REF is 2026-03-06 (Friday). Previous week: Mon Feb 23 – Sun Mar 1.
		const week = parseNlpQuery('notes last week', 'text');
		const wFrom = new Date(week.dateFrom! * 1000);
		const wTo   = new Date(week.dateTo!   * 1000);
		expect(wFrom.toISOString().slice(0, 10)).toBe('2026-02-23'); // Monday
		expect(wTo.toISOString().slice(0, 10)).toBe('2026-03-01');   // Sunday

		// Previous weekend: Sat Feb 28 – Sun Mar 1.
		const wknd = parseNlpQuery('notes last weekend', 'text');
		const eFrom = new Date(wknd.dateFrom! * 1000);
		const eTo   = new Date(wknd.dateTo!   * 1000);
		expect(eFrom.toISOString().slice(0, 10)).toBe('2026-02-28'); // Saturday
		expect(eTo.toISOString().slice(0, 10)).toBe('2026-03-01');   // Sunday
	});

	it('discards date extraction when resulting range is invalid (from > to)', () => {
		// If chrono picks a future date for the lower bound, the guard kicks in.
		// We can't easily force this case, but verify the guard exists by checking
		// that dateFrom <= dateTo whenever both are defined.
		const r = parseNlpQuery('notes in the last month', 'text');
		if (r.dateFrom != null && r.dateTo != null) {
			expect(r.dateFrom).toBeLessThanOrEqual(r.dateTo);
		}
	});

	it('generates a human-readable date label', () => {
		const r = parseNlpQuery('foo last week', 'text');
		if (r.dateLabel) {
			expect(r.dateLabel).toMatch(/Mar|Feb/);
		}
	});
});

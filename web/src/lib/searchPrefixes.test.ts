import { describe, it, expect } from 'vitest';
import { parseSearchPrefixes, toServerMode, hasSearchableContent } from './searchPrefixes';
import type { SearchScope, SearchMatchType, PrefixToken } from './searchPrefixes';

/** Simulate clicking ✕ on a chip: replaces the raw token with its value in the query. */
function simulateRemove(query: string, token: PrefixToken): string {
	const parts = query.split(/\s+/);
	return parts
		.flatMap((t) => (t === token.raw ? (token.value ? [token.value] : []) : [t]))
		.join(' ');
}

describe('parseSearchPrefixes', () => {
	// ── Basic scope prefixes ──────────────────────────────────────────────────

	it('file: sets scope=file', () => {
		const r = parseSearchPrefixes('file:invoice');
		expect(r.scopeOverride).toBe('file');
		expect(r.matchOverride).toBeNull();
		expect(r.query).toBe('invoice');
		expect(r.prefixTokens).toHaveLength(1);
		expect(r.prefixTokens[0].scope).toBe('file');
		expect(r.prefixTokens[0].value).toBe('invoice');
	});

	it('doc: sets scope=doc', () => {
		const r = parseSearchPrefixes('doc:meeting notes');
		expect(r.scopeOverride).toBe('doc');
		expect(r.matchOverride).toBeNull();
		expect(r.query).toBe('meeting notes');
	});

	it('document: is alias for doc:', () => {
		const r = parseSearchPrefixes('document:meeting notes');
		expect(r.scopeOverride).toBe('doc');
		expect(r.query).toBe('meeting notes');
	});

	it('no prefix leaves scope null', () => {
		const r = parseSearchPrefixes('invoice.pdf');
		expect(r.scopeOverride).toBeNull();
		expect(r.matchOverride).toBeNull();
		expect(r.query).toBe('invoice.pdf');
		expect(r.prefixTokens).toHaveLength(0);
	});

	// ── Basic match-type prefixes ─────────────────────────────────────────────

	it('regex: sets match=regex', () => {
		const r = parseSearchPrefixes('regex:foo.*bar');
		expect(r.matchOverride).toBe('regex');
		expect(r.scopeOverride).toBeNull();
		expect(r.query).toBe('foo.*bar');
	});

	it('exact: sets match=exact', () => {
		const r = parseSearchPrefixes('exact:error 500');
		expect(r.matchOverride).toBe('exact');
		expect(r.scopeOverride).toBeNull();
		expect(r.query).toBe('error 500');
	});

	// ── Compound tokens — both orderings equivalent ───────────────────────────

	it('file:exact: → scope=file, match=exact', () => {
		const r = parseSearchPrefixes('file:exact:invoice.pdf');
		expect(r.scopeOverride).toBe('file');
		expect(r.matchOverride).toBe('exact');
		expect(r.query).toBe('invoice.pdf');
		expect(r.prefixTokens[0].value).toBe('invoice.pdf');
	});

	it('exact:file: → same as file:exact:', () => {
		const r = parseSearchPrefixes('exact:file:invoice.pdf');
		expect(r.scopeOverride).toBe('file');
		expect(r.matchOverride).toBe('exact');
		expect(r.query).toBe('invoice.pdf');
	});

	it('doc:regex: → scope=doc, match=regex', () => {
		const r = parseSearchPrefixes('doc:regex:fn\\s+\\w+');
		expect(r.scopeOverride).toBe('doc');
		expect(r.matchOverride).toBe('regex');
		expect(r.query).toBe('fn\\s+\\w+');
	});

	it('regex:doc: → same as doc:regex:', () => {
		const r = parseSearchPrefixes('regex:doc:fn\\s+\\w+');
		expect(r.scopeOverride).toBe('doc');
		expect(r.matchOverride).toBe('regex');
		expect(r.query).toBe('fn\\s+\\w+');
	});

	// ── Kind filter ───────────────────────────────────────────────────────────

	it('type:image sets kindsOverride', () => {
		const r = parseSearchPrefixes('type:image sunset');
		expect(r.kindsOverride).toEqual(['image']);
		expect(r.query).toBe('sunset');
	});

	it('multiple type: tokens accumulate', () => {
		const r = parseSearchPrefixes('type:pdf type:image hello');
		expect(r.kindsOverride).toEqual(['pdf', 'image']);
		expect(r.query).toBe('hello');
	});

	it('type:image combined with file:exact:', () => {
		const r = parseSearchPrefixes('type:image file:exact:*.jpg');
		expect(r.kindsOverride).toEqual(['image']);
		expect(r.scopeOverride).toBe('file');
		expect(r.matchOverride).toBe('exact');
		expect(r.query).toBe('*.jpg');
	});

	// ── Unknown / pass-through ────────────────────────────────────────────────

	it('unknown prefix passes through as literal', () => {
		const r = parseSearchPrefixes('greeting:hello world');
		expect(r.scopeOverride).toBeNull();
		expect(r.matchOverride).toBeNull();
		expect(r.query).toBe('greeting:hello world');
		expect(r.prefixTokens).toHaveLength(0);
	});

	it('type: with unknown kind passes through', () => {
		const r = parseSearchPrefixes('type:unicorn');
		expect(r.kindsOverride).toBeNull();
		expect(r.query).toBe('type:unicorn');
	});

	it('type: with space is literal (two tokens)', () => {
		const r = parseSearchPrefixes('type: image');
		expect(r.kindsOverride).toBeNull();
		// "type:" has empty kind → literal; "image" is literal
		expect(r.query).toBe('type: image');
	});

	// ── Conflict resolution ───────────────────────────────────────────────────

	it('multiple scope prefixes: last wins', () => {
		const r = parseSearchPrefixes('file:invoice doc:report');
		expect(r.scopeOverride).toBe('doc');
		expect(r.query).toBe('invoice report');
	});

	it('multiple match prefixes: last wins', () => {
		const r = parseSearchPrefixes('regex:foo exact:bar');
		expect(r.matchOverride).toBe('exact');
		expect(r.query).toBe('foo bar');
	});

	// ── Safety fallback ───────────────────────────────────────────────────────

	it('type:image alone falls back to raw query', () => {
		const r = parseSearchPrefixes('type:image');
		expect(r.query).toBe('type:image');
	});

	it('file:exact: with empty value falls back to raw', () => {
		const r = parseSearchPrefixes('file:exact:');
		expect(r.query).toBe('file:exact:');
	});

	it('doc:regex: with empty value falls back to raw', () => {
		const r = parseSearchPrefixes('doc:regex:');
		expect(r.query).toBe('doc:regex:');
	});

	// ── Case insensitivity ────────────────────────────────────────────────────

	it('FILE:EXACT: is case-insensitive', () => {
		const r = parseSearchPrefixes('FILE:EXACT:foo');
		expect(r.scopeOverride).toBe('file');
		expect(r.matchOverride).toBe('exact');
		expect(r.query).toBe('foo');
	});

	// ── Quoted strings ────────────────────────────────────────────────────────

	it('regex: with quoted string preserves quotes', () => {
		const r = parseSearchPrefixes('regex:"foo bar"');
		expect(r.matchOverride).toBe('regex');
		expect(r.query).toBe('"foo bar"');
	});

	// ── Edge cases ────────────────────────────────────────────────────────────

	it('file:doc: last scope wins (doc)', () => {
		const r = parseSearchPrefixes('file:doc:report');
		expect(r.scopeOverride).toBe('doc');
		expect(r.query).toBe('report');
	});

	it('type:image: does not compound (colon in kind value → unknown)', () => {
		// type:image:exact:foo — kind would be "image:exact:foo" which contains ':'
		const r = parseSearchPrefixes('type:image:exact:foo');
		expect(r.kindsOverride).toBeNull();
		// Passes through as literal
		expect(r.query).toBe('type:image:exact:foo');
	});
});

// ── Chip removal (simulateRemove) ─────────────────────────────────────────────

describe('chip removal', () => {
	it('file:extra components — removing chip keeps "extra"', () => {
		const query = 'file:extra components';
		const r = parseSearchPrefixes(query);
		const chip = r.prefixTokens[0];
		expect(chip.raw).toBe('file:extra');
		expect(chip.value).toBe('extra');
		expect(simulateRemove(query, chip)).toBe('extra components');
	});

	it('file: alone — removing chip leaves empty query', () => {
		// "file:" with no value: rest is empty → removing chip produces empty string
		const query = 'file:';
		const r = parseSearchPrefixes(query);
		// safety fallback kicks in (empty query), but chip raw should still be 'file:'
		const chip = r.prefixTokens[0];
		expect(chip.value).toBe('');
		expect(simulateRemove(query, chip)).toBe('');
	});

	it('file:exact:name.txt — removing chip keeps "name.txt"', () => {
		const query = 'file:exact:name.txt';
		const r = parseSearchPrefixes(query);
		const chip = r.prefixTokens[0];
		expect(chip.value).toBe('name.txt');
		expect(simulateRemove(query, chip)).toBe('name.txt');
	});

	it('type:image sunset — removing chip leaves "sunset"', () => {
		const query = 'type:image sunset';
		const r = parseSearchPrefixes(query);
		const chip = r.prefixTokens[0];
		expect(chip.value).toBe('');
		expect(simulateRemove(query, chip)).toBe('sunset');
	});

	it('standalone file: — removing chip leaves empty string', () => {
		const query = 'file: hello';
		const r = parseSearchPrefixes(query);
		// "file:" is one token with empty value; "hello" is a separate literal token
		expect(simulateRemove(query, r.prefixTokens[0])).toBe('hello');
	});
});

// ── source: prefix ──────────────────────────────────────────────────────────────

describe('source: prefix', () => {
	it('parses source + path', () => {
		const r = parseSearchPrefixes('source:nas-data/multimedia/movies hello');
		expect(r.dirSource).toBe('nas-data');
		expect(r.dirPrefix).toBe('multimedia/movies');
		expect(r.dirPrefixError).toBeNull();
		expect(r.query).toBe('hello');
		expect(r.prefixTokens).toHaveLength(1);
		expect(r.prefixTokens[0].dirSource).toBe('nas-data');
		expect(r.prefixTokens[0].dirPrefix).toBe('multimedia/movies');
	});

	it('source only (no path) gives empty dirPrefix', () => {
		const r = parseSearchPrefixes('source:nas-data hello');
		expect(r.dirSource).toBe('nas-data');
		expect(r.dirPrefix).toBe('');
		expect(r.dirPrefixError).toBeNull();
	});

	it('strips leading slash', () => {
		const r = parseSearchPrefixes('source:/nas-data/multimedia hello');
		expect(r.dirSource).toBe('nas-data');
		expect(r.dirPrefix).toBe('multimedia');
	});

	it('strips trailing slash', () => {
		const r = parseSearchPrefixes('source:nas-data/multimedia/ hello');
		expect(r.dirSource).toBe('nas-data');
		expect(r.dirPrefix).toBe('multimedia');
	});

	it('empty after normalisation produces error', () => {
		const r = parseSearchPrefixes('source:/ hello');
		expect(r.dirSource).toBeNull();
		expect(r.dirPrefixError).not.toBeNull();
	});

	it('bare source: produces error', () => {
		const r = parseSearchPrefixes('source: hello');
		expect(r.dirSource).toBeNull();
		expect(r.dirPrefixError).not.toBeNull();
	});

	it('preserves original casing', () => {
		const r = parseSearchPrefixes('source:Nas-Data/Multimedia/Movies');
		expect(r.dirSource).toBe('Nas-Data');
		expect(r.dirPrefix).toBe('Multimedia/Movies');
	});

	it('absent when no source: token', () => {
		const r = parseSearchPrefixes('hello world');
		expect(r.dirSource).toBeNull();
		expect(r.dirPrefix).toBeNull();
		expect(r.dirPrefixError).toBeNull();
	});
});

// ── hasSearchableContent ──────────────────────────────────────────────────────

describe('hasSearchableContent', () => {
	it('plain query with 3+ chars is searchable', () => {
		expect(hasSearchableContent('foo')).toBe(true);
	});
	it('plain query under 3 chars is not searchable', () => {
		expect(hasSearchableContent('fo')).toBe(false);
	});
	it('prefix-only query is not searchable', () => {
		expect(hasSearchableContent('doc:')).toBe(false);
		expect(hasSearchableContent('source:nas-data/path')).toBe(false);
		expect(hasSearchableContent('type:pdf')).toBe(false);
	});
	it('prefix + short free text is not searchable', () => {
		expect(hasSearchableContent('doc: fo')).toBe(false);
	});
	it('prefix + sufficient free text is searchable', () => {
		expect(hasSearchableContent('doc:foo')).toBe(true);
		expect(hasSearchableContent('source:nas-data/path foo')).toBe(true);
		expect(hasSearchableContent('type:pdf invoice')).toBe(true);
	});
	it('multiple prefixes no free text is not searchable', () => {
		expect(hasSearchableContent('doc: source:nas-data/foo')).toBe(false);
	});
});

// ── toServerMode ─────────────────────────────────────────────────────────────

describe('toServerMode', () => {
	const cases: [SearchScope, SearchMatchType, string][] = [
		['line', 'fuzzy',  'fuzzy'],
		['line', 'exact',  'exact'],
		['line', 'regex',  'regex'],
		['file', 'fuzzy',  'file-fuzzy'],
		['file', 'exact',  'file-exact'],
		['file', 'regex',  'file-regex'],
		['doc',  'fuzzy',  'document'],
		['doc',  'exact',  'doc-exact'],
		['doc',  'regex',  'doc-regex'],
	];
	for (const [scope, match, expected] of cases) {
		it(`${scope}+${match} → ${expected}`, () => {
			expect(toServerMode(scope, match)).toBe(expected);
		});
	}
});

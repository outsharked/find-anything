import { KIND_OPTIONS } from './kindOptions';

export type SearchScope = 'line' | 'file' | 'doc';
export type SearchMatchType = 'fuzzy' | 'exact' | 'regex';

export interface PrefixToken {
	raw: string;                  // complete original token (for chip display and removal)
	value: string;                // non-prefix remainder; empty string when entire token is a prefix
	scope: SearchScope | null;
	match: SearchMatchType | null;
	kind: string | null;          // set for type: tokens
}

export interface PrefixParseResult {
	query: string;                    // prefixes stripped; free-text tokens joined
	scopeOverride: SearchScope | null;
	matchOverride: SearchMatchType | null;
	kindsOverride: string[] | null;   // null = use UI state
	prefixTokens: PrefixToken[];      // for chips
}

const SCOPE_MAP: Record<string, SearchScope> = {
	file: 'file',
	doc: 'doc',
	document: 'doc',
};

const MATCH_MAP: Record<string, SearchMatchType> = {
	exact: 'exact',
	regex: 'regex',
};

const KIND_SET = new Set(KIND_OPTIONS.map((k) => k.value));

/** Split `raw` on whitespace while respecting double-quoted substrings. */
function tokenize(raw: string): string[] {
	const tokens: string[] = [];
	let cur = '';
	let inQuote = false;
	for (const ch of raw) {
		if (ch === '"' && !inQuote) {
			inQuote = true;
			cur += ch;
		} else if (ch === '"' && inQuote) {
			inQuote = false;
			cur += ch;
		} else if ((ch === ' ' || ch === '\t') && !inQuote) {
			if (cur) { tokens.push(cur); cur = ''; }
		} else {
			cur += ch;
		}
	}
	if (cur) tokens.push(cur);
	return tokens;
}

export function parseSearchPrefixes(raw: string): PrefixParseResult {
	const tokens = tokenize(raw.trim());

	let scopeOverride: SearchScope | null = null;
	let matchOverride: SearchMatchType | null = null;
	const kindsFound: string[] = [];
	const prefixTokens: PrefixToken[] = [];
	const queryFragments: string[] = [];

	for (const token of tokens) {
		const lower = token.toLowerCase();

		// type: prefix (single-level, takes kind value — cannot compound with scope/match)
		if (lower.startsWith('type:')) {
			const kindName = lower.slice(5);
			if (kindName && !kindName.includes(':') && KIND_SET.has(kindName)) {
				kindsFound.push(kindName);
				prefixTokens.push({ raw: token, value: '', scope: null, match: null, kind: kindName });
				continue;
			}
			// Unknown kind → treat as literal
			queryFragments.push(token);
			continue;
		}

		// Try to parse compound scope/match prefixes (e.g. "file:exact:" or "regex:doc:")
		let tokenScope: SearchScope | null = null;
		let tokenMatch: SearchMatchType | null = null;
		let rest = token;

		while (rest.includes(':')) {
			const colon = rest.indexOf(':');
			const seg = rest.slice(0, colon).toLowerCase();
			if (seg in SCOPE_MAP) {
				tokenScope = SCOPE_MAP[seg]; // last within token wins
				rest = rest.slice(colon + 1);
			} else if (seg in MATCH_MAP) {
				tokenMatch = MATCH_MAP[seg]; // last within token wins
				rest = rest.slice(colon + 1);
			} else {
				break; // not a recognised prefix — stop
			}
		}

		if (tokenScope !== null || tokenMatch !== null) {
			// This token had at least one recognised prefix; last token's value wins overall
			if (tokenScope !== null) scopeOverride = tokenScope;
			if (tokenMatch !== null) matchOverride = tokenMatch;
			prefixTokens.push({ raw: token, value: rest, scope: tokenScope, match: tokenMatch, kind: null });
			if (rest) queryFragments.push(rest);
		} else {
			// No recognised prefix — treat as literal query text
			queryFragments.push(token);
		}
	}

	// Safety fallback: if stripping all prefixes leaves empty query, use raw
	let query = queryFragments.join(' ');
	if (!query.trim()) query = raw.trim();

	return {
		query,
		scopeOverride,
		matchOverride,
		kindsOverride: kindsFound.length > 0 ? kindsFound : null,
		prefixTokens,
	};
}

/** Compose scope + matchType into the server's mode string. */
export function toServerMode(scope: SearchScope, match: SearchMatchType): string {
	if (scope === 'line') {
		if (match === 'fuzzy') return 'fuzzy';
		if (match === 'exact') return 'exact';
		return 'regex';
	}
	if (scope === 'file') {
		if (match === 'fuzzy') return 'file-fuzzy';
		if (match === 'exact') return 'file-exact';
		return 'file-regex';
	}
	// doc
	if (match === 'fuzzy') return 'document';
	if (match === 'exact') return 'doc-exact';
	return 'doc-regex';
}

/** Parse a server mode string back into scope + matchType. */
export function fromServerMode(mode: string): { scope: SearchScope; matchType: SearchMatchType } {
	switch (mode) {
		case 'fuzzy':      return { scope: 'line', matchType: 'fuzzy' };
		case 'exact':      return { scope: 'line', matchType: 'exact' };
		case 'regex':      return { scope: 'line', matchType: 'regex' };
		case 'file-fuzzy': return { scope: 'file', matchType: 'fuzzy' };
		case 'file-exact': return { scope: 'file', matchType: 'exact' };
		case 'file-regex': return { scope: 'file', matchType: 'regex' };
		case 'document':   return { scope: 'doc',  matchType: 'fuzzy' };
		case 'doc-exact':  return { scope: 'doc',  matchType: 'exact' };
		case 'doc-regex':  return { scope: 'doc',  matchType: 'regex' };
		default:           return { scope: 'line', matchType: 'fuzzy' };
	}
}

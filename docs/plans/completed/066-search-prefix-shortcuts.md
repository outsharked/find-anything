# 066 — Search Bar Prefix Shortcuts

## Overview

Allow users to type structured prefixes in the search bar to control search
scope, match type, and kind filter — without touching the Advanced panel.
Prefixes are parsed client-side before the API call. They compose freely with
each other and with the existing UI controls.

### Scope prefixes

| Prefix | Scope |
|--------|-------|
| *(none)* | Single-line (default) |
| `file:` | Filename only (`line_number = 0` rows) |
| `doc:` / `document:` | Whole-file / document mode |

### Match-type prefixes

| Prefix | Match type |
|--------|------------|
| *(none)* | Fuzzy (default) |
| `exact:` | Exact phrase |
| `regex:` | Regular expression |

### Kind filter

| Prefix | Effect |
|--------|--------|
| `type:<kind>` | Restrict to files of that kind (image, pdf, audio, …) |

### Compounding

Scope and match-type prefixes can be combined in a single token (no space) in
any order. Both orderings are equivalent:

```
file:exact:invoice.pdf    ≡   exact:file:invoice.pdf
doc:regex:fn\s+\w+        ≡   regex:doc:fn\s+\w+
file:regex:.*\.pdf$       ≡   regex:file:.*\.pdf$
```

`type:` is always a separate token (it takes a kind-name value, not a modifier).

### Full examples

```
invoice.pdf                            → scope=line,  match=fuzzy,  query="invoice.pdf"
file:invoice                           → scope=file,  match=fuzzy,  query="invoice"
file:exact:invoice.pdf                 → scope=file,  match=exact,  query="invoice.pdf"
exact:file:invoice.pdf                 → same (order-agnostic)
exact:file:foo.txt                     → scope=file,  match=exact,  query="foo.txt"
file:regex:.*parrot.*\.jpg             → scope=file,  match=regex,  query=".*parrot.*\.jpg"
regex:file:.*parrot.*\.jpg             → same (order-agnostic)
file:regex:.*\.pdf$                    → scope=file,  match=regex,  query=".*\.pdf$"
doc:meeting notes                      → scope=doc,   match=fuzzy,  query="meeting notes"
doc:exact:null pointer                 → scope=doc,   match=exact,  query="null pointer"
exact:doc:null pointer                 → same (order-agnostic)
doc:regex:fn\s+\w+                     → scope=doc,   match=regex,  query="fn\s+\w+"
regex:.*parrot.*\.jpg                  → scope=line,  match=regex,  query=".*parrot.*\.jpg"
exact:error code 500                   → scope=line,  match=exact,  query="error code 500"
type:image regex:.*parrot.*\.jpg       → scope=line,  match=regex,  kind=image,  query=".*parrot.*\.jpg"
type:image file:regex:.*\.jpg          → scope=file,  match=regex,  kind=image,  query=".*\.jpg"
type:image file:regex:.*parrot.*\.jpg  → scope=file,  match=regex,  kind=image,  query=".*parrot.*\.jpg"
type:pdf doc:annual report             → scope=doc,   match=fuzzy,  kind=pdf,    query="annual report"
type:pdf doc:exact:Q4 earnings         → scope=doc,   match=exact,  kind=pdf,    query="Q4 earnings"
```

---

## Prerequisite — `[PATH]` prefix on path lines (plan 067)

The `file-*` server modes added in Step 5 filter `AND l.line_number = 0` to restrict
matches to filename rows. This is ambiguous: `line_number=0` is a shared metadata
bucket that also holds EXIF tags, audio tags, PE version strings, MIME fallback lines,
etc. A search for `file:canon` would incorrectly match images whose EXIF contains
`[EXIF:Make] Canon`, and a search for `file:notepad` would match PE metadata
`ProductName: Notepad`.

**Plan 067** — *Standardise `line_number=0` metadata prefixes* — fixes this by:
1. Adding `[PATH] ` prefix to all path lines (was bare `"src/invoice.pdf"`).
2. Converting PE metadata from `"Key: Value"` to `"[PE:Key] Value"`.
3. Updating the `file-*` SQL filter from `AND l.line_number = 0` to
   `AND l.line_number = 0 AND l.content LIKE '[PATH] %'`.
4. Stripping `[PATH] ` from result snippets server-side.

**Until plan 067 lands, the `file-*` modes in this plan are partially broken** for
files that have non-path metadata at line_number=0 (images with EXIF, audio with
tags, PE binaries). The UI and prefix-parsing logic are fully correct; only the
server-side SQL filter needs tightening.

---

## Design Decisions

### 1. Parsing is client-side only

No server-side changes are needed for the prefix feature itself (except adding
`filename` mode — see §5). The server already accepts `mode` and `kind[]`
params. `parseSearchPrefixes()` runs before the API call, the same way
`parseNlpQuery` does.

The raw query (with prefixes) is stored in the `query` state variable unchanged.
The parsed result (clean query + extracted scope/match/kinds) is used for the
API call only. URL sharing preserves the full prefix string in `?q=`.

### 2. Prefixes override (not merge with) UI state

When a scope/match prefix is present it overrides the corresponding Advanced
panel control for that search call. When absent, the panel's values apply.
`type:` overrides `selectedKinds`; when absent `selectedKinds` from the panel
applies.

### 3. Raw input is displayed verbatim

Same pattern as `nlpQuery`: the raw query owns the prefix tokens; the
effective (stripped) query is only computed at call time. The panel toggles
reflect the *persistent* UI state, not the prefix override.

### 4. Search-type dropdown → two toggle groups in Advanced

The `<select>` in `SearchBox.svelte` is removed. Because scope and match type
are now orthogonal, the Advanced panel gets **two separate toggle groups**:

**Scope:** Single-line | Filename | Document
**Match type:** Fuzzy | Exact | Regex

The persistent state becomes `{ scope, matchType }` replacing the single `mode`
string. The server still receives a single `mode` string; client composes it
from the two selections before the API call (e.g. scope=doc + matchType=regex →
`mode = "doc-regex"`; see §5 for how the server handles each combination).

### 5. Server modes — the 3×3 matrix

| | fuzzy | exact | regex |
|---|---|---|---|
| **line** | `fuzzy` (exists) | `exact` (exists) | `regex` (exists) |
| **file** | `file-fuzzy` (new) | `file-exact` (new) | `file-regex` (new) |
| **doc** | `document` (exists) | `doc-exact` (new) | `doc-regex` (new) |

New modes are all additive. No `MIN_CLIENT_VERSION` bump.

**file-\* modes**: Add `AND l.line_number = 0 AND l.content LIKE '[PATH] %'` to the
`fts_candidates` SQL (requires plan 067 prefix convention to be in place; until then
the filter is `AND l.line_number = 0` which matches all metadata rows).
Match type is applied exactly as for the line-scoped equivalent — the only
difference is the row filter.

**doc-exact**: Use `document_candidates` but replace the per-token FTS5 queries
with phrase queries (`"exact phrase"` instead of `"token"`). The file-ID
intersection logic is unchanged.

**doc-regex**: Use `document_candidates` to find qualifying files (regex literal
fragments extracted via `regex_to_fts_terms` as the FTS pre-filter), then apply
`re.is_match()` to the representative and extra-match lines in the second pass.

Cross-line regex (pattern spanning line boundaries) is **out of scope** — lines
are stored individually and concatenation is not practical at query time.

### 6. Prefix parsing rules

- Tokenise on whitespace; preserve quoted strings.
- Each token is examined for a leading run of recognised prefix names separated
  by `:`. A "recognised prefix name" is one of: `file`, `doc`, `document`,
  `exact`, `regex`, `type`.
- Parsing stops when a segment is not a recognised prefix name. The remainder
  of the token (after the last `:`) is a query fragment.
- **`type:` special case**: always a single-level prefix. The value after the
  colon is the kind name. If the kind name is not in KIND_OPTIONS, the whole
  token passes through as literal text.
- Prefix matching is case-insensitive.
- Multiple `type:` tokens accumulate into the kinds list.
- Multiple scope prefixes in separate tokens: last wins. Multiple match-type
  prefixes in separate tokens: last wins.
- Within a single compound token (e.g. `file:exact:`), duplicate or conflicting
  sub-prefixes: last recognised one of each category wins.
- `type:` cannot be compounded with scope/match prefixes (it takes a value
  argument, not a modifier role). `type:image:exact:...` is not valid — only
  `type:image` is recognised; `exact:...` would need to be a separate token.
- If stripping all prefixes leaves an empty query, fall back to the full raw
  string (same safety guard as `parseNlpQuery`).

### 7. Prefix chips in the search area

Below the search box, show a chip row for active prefixes, following the
existing `nlp-bar` / `nlp-chip` pattern in `SearchView.svelte`. Each chip has
an ✕ that splices its raw token out of the query string and re-runs the search.

---

## Implementation

### Step 1 — Extract `KIND_OPTIONS` to a shared module

Create `web/src/lib/kindOptions.ts` and import it in `AdvancedSearch.svelte`
and the new `searchPrefixes.ts`.

### Step 2 — Create `web/src/lib/searchPrefixes.ts`

```typescript
export type SearchScope = 'line' | 'file' | 'doc';
export type SearchMatchType = 'fuzzy' | 'exact' | 'regex';

export interface PrefixParseResult {
  query: string;                      // prefixes stripped; free-text tokens joined
  scopeOverride: SearchScope | null;
  matchOverride: SearchMatchType | null;
  kindsOverride: string[] | null;     // null = use UI state
  prefixTokens: PrefixToken[];        // for chips
}

export interface PrefixToken {
  raw: string;                        // original token, e.g. "file:exact:invoice.pdf"
  scope: SearchScope | null;
  match: SearchMatchType | null;
  kind: string | null;                // set for type: tokens
}

export function parseSearchPrefixes(raw: string): PrefixParseResult;

/** Compose scope + matchType into the server's mode string. */
export function toServerMode(scope: SearchScope, match: SearchMatchType): string;
```

`toServerMode` mapping:

| scope | match | server mode |
|-------|-------|-------------|
| line | fuzzy | `"fuzzy"` |
| line | exact | `"exact"` |
| line | regex | `"regex"` |
| file | fuzzy | `"file-fuzzy"` |
| file | exact | `"file-exact"` |
| file | regex | `"file-regex"` |
| doc | fuzzy | `"document"` |
| doc | exact | `"doc-exact"` |
| doc | regex | `"doc-regex"` |

### Step 3 — Wire into `doSearch` in `+page.svelte`

The existing `mode` string state variable becomes `{ scope: SearchScope, matchType: SearchMatchType }`.

```typescript
const prefixResult = parseSearchPrefixes(q);
const effectiveScope    = prefixResult.scopeOverride  ?? scope;
const effectiveMatch    = prefixResult.matchOverride  ?? matchType;
const effectiveKinds    = prefixResult.kindsOverride  ?? selectedKinds;
const serverMode        = toServerMode(effectiveScope, effectiveMatch);
const baseQuery         = prefixResult.query;

nlpResult = nlpSuppressed ? null : parseNlpQuery(baseQuery, serverMode);
// API call uses serverMode, effectiveKinds, nlpResult.query
```

### Step 4 — Add prefix chips in `SearchView.svelte`

Same position as the NLP date bar. Each chip shows the compound prefix name
(e.g. "file · exact") and an ✕ to remove it.

### Step 5 — Add new modes to the server

**`crates/server/src/db/search.rs`**:
- Add `filename_only: bool` to the filter struct used by `fts_candidates`.
  When true, append `AND l.line_number = 0`.

**`crates/server/src/routes/search.rs`**:
- Parse `mode` string into `(scope, match_type)`.
- Dispatch `fts_candidates` with `filename_only = true` for `file-*` modes.
- For `doc-exact`: call `document_candidates` with phrase-mode per token.
- For `doc-regex`: call `document_candidates` with regex literal fragments as
  FTS pre-filter; apply `re.is_match()` in the post-filter pass.

### Step 6 — Remove mode `<select>` from `SearchBox.svelte`

Remove the `<select class="mode-select">` and `handleModeChange`. The `mode`
prop becomes `scope + matchType` props if needed for display, or is dropped if
the component no longer needs it.

### Step 7 — Add two toggle groups to `AdvancedSearch.svelte`

```
Scope       [Single-line]  [Filename]  [Document]
Match type  [Fuzzy]  [Exact]  [Regex]
```

`AdvancedSearch` gains `scope: SearchScope` and `matchType: SearchMatchType`
props. Both are included in the `change` event payload. `+page.svelte`
`handleFilterChange` updates the two state variables. `SearchView.svelte` and
`FileView.svelte` pass them down.

---

## Files Changed

### Web

| File | Change |
|------|--------|
| `web/src/lib/kindOptions.ts` | **NEW** — shared KIND_OPTIONS |
| `web/src/lib/searchPrefixes.ts` | **NEW** — `parseSearchPrefixes`, `toServerMode` |
| `web/src/lib/searchPrefixes.test.ts` | **NEW** — unit tests |
| `web/src/lib/SearchBox.svelte` | Remove mode `<select>` |
| `web/src/lib/AdvancedSearch.svelte` | Replace mode select with two toggle groups; add `scope`+`matchType` props; include both in change event |
| `web/src/lib/SearchView.svelte` | Pass `scope`+`matchType` to AdvancedSearch; add prefix chip bar |
| `web/src/lib/FileView.svelte` | Pass `scope`+`matchType` to AdvancedSearch |
| `web/src/routes/+page.svelte` | Replace `mode` string state with `scope`+`matchType`; wire `parseSearchPrefixes`; handle both in `handleFilterChange` |

### Server (Rust)

| File | Change |
|------|--------|
| `crates/server/src/db/search.rs` | Add `filename_only` to filter struct; apply `AND l.line_number = 0` when set |
| `crates/server/src/routes/search.rs` | Parse new mode strings; dispatch `file-*`, `doc-exact`, `doc-regex` paths |

---

## Testing Strategy

### Unit tests (`searchPrefixes.test.ts`)

**Basic scope prefixes**
- `file:invoice` → scope=file, match=null, query=`"invoice"`
- `doc:meeting notes` → scope=doc, match=null, query=`"meeting notes"`
- `document:meeting notes` → same as `doc:`
- `invoice.pdf` (no prefix) → scope=null, match=null, query=`"invoice.pdf"`

**Basic match-type prefixes**
- `regex:foo.*bar` → match=regex, scope=null, query=`"foo.*bar"`
- `exact:error 500` → match=exact, scope=null, query=`"error 500"`

**Compound tokens — both orderings equivalent**
- `file:exact:invoice.pdf` → scope=file, match=exact, query=`"invoice.pdf"`
- `exact:file:invoice.pdf` → same
- `doc:regex:fn\s+\w+` → scope=doc, match=regex, query=`"fn\s+\w+"`
- `regex:doc:fn\s+\w+` → same

**Kind filter**
- `type:image sunset` → kinds=`['image']`, query=`"sunset"`
- `type:pdf type:image hello` → kinds=`['pdf','image']`, query=`"hello"`
- `type:image file:exact:*.jpg` → kinds=`['image']`, scope=file, match=exact, query=`"*.jpg"`

**Unknown / pass-through**
- `greeting:hello world` → no extraction, query=`"greeting:hello world"`
- `type:unicorn` → unknown kind, pass through as literal
- `type: image` (space) → literal

**Conflict resolution**
- `file:invoice doc:report` → last scope wins: scope=doc, query=`"invoice report"`
- `regex:foo exact:bar` → last match wins: match=exact, query=`"foo bar"`

**Safety fallback**
- `type:image` alone → query falls back to raw `"type:image"`

**Case insensitivity**
- `FILE:EXACT:foo` → scope=file, match=exact, query=`"foo"`

**Quoted strings**
- `regex:"foo bar"` → query=`'"foo bar"'`

### Server integration tests

- `mode=file-fuzzy&q=invoice` → returns file named `invoice.pdf`; does NOT return file whose content contains "invoice" but name does not
- `mode=file-exact&q=invoice.pdf` → exact filename match
- `mode=file-regex&q=.*\.pdf$` → filename regex match
- `mode=doc-exact&q=null pointer` → document contains both words (per-file match)
- `mode=doc-regex&q=fn\s+\w+` → document has a line matching the regex

### Manual checklist

- [ ] `file:invoice` → Scope=Filename, Match=Fuzzy highlighted in Advanced
- [ ] `file:exact:invoice.pdf` → Scope=Filename, Match=Exact highlighted
- [ ] `exact:file:invoice.pdf` → same result
- [ ] `type:image file:regex:.*\.jpg` → kind chip + scope+match chips; correct search
- [ ] ✕ on a prefix chip → prefix removed, search re-runs
- [ ] Scope/Match toggles in Advanced persist across searches
- [ ] Prefix overrides Advanced: Advanced=Fuzzy, type `regex:foo` → runs as regex
- [ ] URL round-trip: `?q=file:exact:invoice.pdf` parsed correctly on load

---

## Edge Cases

| Scenario | Behaviour |
|----------|-----------|
| `file:doc:report` | `file` = scope, `doc` = scope conflict — last wins: scope=doc, query=`"report"` |
| `type:image:exact:foo` | Only `type:image` is recognised; `:exact:foo` is the kind value → unknown kind → literal |
| `file:exact:` (empty value) | Safety fallback: query = raw |
| `doc:regex:` (empty value) | Safety fallback: query = raw |
| True multi-line regex | Out of scope — lines stored individually |

---

## Breaking Changes

None. All changes are additive:
- New prefix syntax; existing queries unaffected
- New server mode strings are additive; no existing paths changed
- `MIN_CLIENT_VERSION` does not need bumping
- Advanced panel changes are cosmetic re-arrangements of existing functionality

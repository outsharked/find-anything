# Natural Language Date Parsing

## Overview

Allow users to embed date constraints directly in their search query using natural
language. "artifactory in the last two days" should find files matching
"artifactory" with mtime in the last 48 hours. The date phrase is extracted
client-side, removed from the query string, and passed as `date_from`/`date_to`
to the existing API (plan 043). No server changes required.

A secondary concern is stop word removal: articles and conjunctions like "the",
"and" appear in natural-language queries but add noise to FTS5. These are stripped
from the unquoted portion of FTS queries (not regex, not exact mode).

---

## Design Decisions

### Client-side vs. server-side parsing

Date phrase extraction happens **entirely in the browser** using `chrono-node`, a
pure-JS NLP date library with no Node.js dependencies. It runs in browsers without
modification.

Reasons to prefer client-side:
- The server already accepts `date_from`/`date_to` as unix timestamp integers — no
  API changes needed.
- Avoids sending ambiguous NL strings to the server; the server always receives
  structured params.
- chrono-node is well-maintained, handles EN locale, and weighs ~60 KB gzipped.

A Rust NL date library could handle this server-side (e.g. `dateparser`, `dtparse`)
but these are far less capable than chrono-node for relative and multi-phrase
expressions ("last two weeks", "between January and March").

### When to apply each transform

| Transform | text/document mode | exact mode | regex mode |
|-----------|-------------------|------------|------------|
| Date extraction | ✅ | ✅ | ❌ |
| Stop word removal | ✅ | ❌ | ❌ |

**Date extraction in exact mode**: the date phrase appears after the content search
terms and is structurally unambiguous. Extracting it is safe and useful. Stop word
removal is suppressed because exact mode users are being deliberate about every
character.

**Regex mode**: the query is a raw regex pattern. Neither transform is applied.

### Stop word list

Conservative — only words that are unambiguous noise between search terms:

```
a  an  the  and
```

`or` and `not` are **not** stripped — they are meaningful FTS5 boolean operators.
Prepositions (`in`, `on`, `from`, `to`, `between`, etc.) are only removed as part
of the date phrase, not as generic stop words, since they can be meaningful content
words ("files in Python", "notes on architecture").

Stop words are only stripped from **unquoted** tokens. `"the quick brown fox"` is
preserved verbatim.

### Date phrase detection strategy

Uses `chrono.parse(text, refDate, { forwardDate: false })` which returns an array
of `ParsedResult` objects, each with `.index`, `.text`, and `.start`/`.end`
`ParsedComponents`.

Three patterns to handle:

**1. Single open-ended reference** — one result, plus a leading preposition:
- "in the last two days" → date_from = now − 2d, date_to = now
- "since Monday" → date_from = last Monday, date_to = now
- "before Christmas" → date_from = none, date_to = Dec 25
- "last week" → date_from = 7d ago, date_to = now
- "yesterday" → date_from = yesterday 00:00, date_to = yesterday 23:59
- "in January" → date_from = Jan 1, date_to = Jan 31

**2. Explicit range** — "between [date1] and [date2]" or "[date1] to [date2]":
Two parsed results found with "between"/"to" connector:
- "between January and February" → date_from = Jan 1, date_to = Feb 28
- "from last Monday to Friday" → date_from = last Mon, date_to = last Fri

**3. No date found** — zero results or ambiguous result → no transform applied.
Ambiguity check: if the matched text overlaps with a quoted segment, skip.

**Removing the date phrase from the query**: identify the span in the original
string that covers the date phrase plus any leading date-linking words (`in`,
`on`, `since`, `before`, `after`, `between`, `from`, `to`, `until`, `last`,
`next`, `the`, `past`, `ago`). Remove that span and trim whitespace.

**Fallback**: if date extraction would empty the entire query (user typed only a
date phrase with no content terms), do not apply — pass the query through unchanged
so the user gets unfiltered results or an error from the server rather than
silently sending an empty query.

### "Between January and February" — is it feasible?

Yes. chrono-node's `parse()` returns both month references as separate
`ParsedResult` items. We detect the "between ... and ..." connector by scanning for
the word "between" before the first result. Each result gives us a
`ParsedComponents` object from which we extract start-of-period / end-of-period:

```ts
// result.start.date() → JS Date at start of implied period
// For a month reference (only year+month known): start = 1st, end = last day
const from = result.start.date();
const to   = result.end?.date() ?? endOfDay(result.start.date());
```

For single-month references like "in January", chrono-node sets `.start` to Jan 1
and `.end` to Jan 31. For "between January and February", two results give us Jan 1
and Feb 28 respectively.

### What requires an LLM (out of scope)

- Semantic intent: "recent financial documents", "stuff I worked on before the
  deadline" — require understanding of meaning, not just date syntax.
- Relative-to-event dates: "files from before the project started".
- Complex boolean temporal logic: "Q3 but not Q4".
- Cross-language date expressions (non-English locales — chrono-node has
  some i18n support but it is limited).

These cases fall through gracefully: chrono-node finds nothing, the query is sent
unchanged, and the existing date picker in the Advanced search panel remains
available for precise control.

### Manual date range vs. NLP-extracted date — precedence and conflict UI

**Manual always wins.** If the user has set a date range via the Advanced search
panel, the NLP-extracted dates are ignored for the actual API call. The manual
date is an explicit, deliberate act; overriding it silently would be surprising.

However, when both are present simultaneously the user should know there is a
conflict. In this state:

- The NLP date chip is still shown (so the user knows a date was detected in
  their query), but it is rendered in a muted/strikethrough style to indicate it
  is not active.
- A red circled exclamation icon `ⓘ` (or `!` in a red circle) is shown inline
  with the chip. On hover, a tooltip reads:
  > "A date was found in your query ("last two days") but a manual date range is
  > also set in Advanced search. The manual range takes precedence. Clear the
  > Advanced date range to use the query date instead."
- The icon and tooltip use standard CSS `title` attribute or a lightweight custom
  tooltip (consistent with the rest of the UI).

No extra user action is required — this is purely informational. The user can
either clear the Advanced date range (letting NLP take over) or remove the date
phrase from their query (eliminating the conflict).

### UI feedback (non-conflict case)

When a date is auto-extracted and no manual date is set, show a dismissible chip
below the search box: `Filtered: Mar 4 – Mar 6 ✕`. Clicking ✕ clears the
auto-detected date range and re-runs the search without it (the raw query is
preserved; only the extracted dates are discarded). This makes the transform
visible and reversible.

---

## Implementation

### Step 1: Install chrono-node

```sh
cd web && pnpm add chrono-node
```

### Step 2: `web/src/lib/nlpQuery.ts`

New module. Exports one function:

```ts
export interface NlpResult {
  query: string;         // cleaned query to send to FTS
  dateFrom?: number;     // unix seconds (inclusive)
  dateTo?: number;       // unix seconds (inclusive)
  dateLabel?: string;    // human-readable label for the chip, e.g. "Mar 4 – Mar 6"
}

export function parseNlpQuery(raw: string, mode: string): NlpResult
```

Internal algorithm:

```
1. If mode === 'regex' → return { query: raw }

2. Extract quoted segments:
   - Find all "..." substrings, replace each with a placeholder ⟨0⟩, ⟨1⟩, …
   - Work on the unquoted remainder

3. Try date extraction on unquoted text:
   a. Call chrono.parse(text, new Date(), options)
   b. If 0 results → no date found
   c. If 1 result → determine if "since/before/after" or open-ended range
      - Check leading words to decide open start vs. open end vs. both
      - Compute dateFrom/dateTo as described above
      - Mark the span (including leading date-linking words) for removal
   d. If 2 results with "between"/"to" connector → range
      - Mark span from "between" to end of second result for removal
   e. If result would be ambiguous (overlaps quoted segment) → skip

4. Remove the marked date span from unquoted text; trim

5. If mode !== 'exact': strip stop words (a, an, the, and) from
   remaining unquoted tokens (split on whitespace, filter, rejoin)

6. Restore quoted placeholders

7. Trim final query; if empty → revert to raw (do not send blank query)

8. Return { query, dateFrom, dateTo, dateLabel }
```

### Step 3: Wire into search flow

**`web/src/lib/SearchView.svelte`** (or wherever `triggerSearch` lives):

Before dispatching a search:
```ts
import { parseNlpQuery } from '$lib/nlpQuery';

const nlp = parseNlpQuery(query, mode);
// Use nlp.query as the FTS query
// Use nlp.dateFrom / nlp.dateTo — these override any manually-set date range
//   only if no manual date range is active; manual always wins
```

Store the `nlp.dateLabel` in local state and show/hide the chip below the search box.

**`web/src/routes/+page.svelte`**: no structural changes needed; the NLP layer
sits entirely within the search dispatch path.

### Step 4: Auto-detected date chip

In `SearchView.svelte`, below the search box:

```svelte
{#if nlpDateLabel}
  <div class="nlp-chip" class:conflict={manualDateActive}>
    <span>Filtered: {nlpDateLabel}</span>
    {#if manualDateActive}
      <span
        class="conflict-icon"
        title="A date was found in your query ("{nlpDateLabel}") but a manual date range is also set in Advanced search. The manual range takes precedence. Clear the Advanced date range to use the query date instead."
        aria-label="Date conflict"
      >!</span>
    {:else}
      <button on:click={clearNlpDate} aria-label="Clear detected date">✕</button>
    {/if}
  </div>
{/if}
```

`manualDateActive` is true when the Advanced panel has a `dateFrom` or `dateTo`
set. In conflict state the chip is visually muted (reduced opacity, text
strikethrough) and the dismiss button is replaced by the red conflict icon.

`clearNlpDate` sets a boolean `nlpSuppressed = true` which causes `parseNlpQuery`
to skip date extraction for the current query. Cleared when the query changes.

---

## Files Changed

| File | Change |
|------|--------|
| `web/package.json` | Add `chrono-node` dependency |
| `web/src/lib/nlpQuery.ts` | New — NLP parse logic |
| `web/src/lib/nlpQuery.test.ts` | New — unit tests |
| `web/src/lib/SearchView.svelte` | Call `parseNlpQuery` before search dispatch; show/hide date chip |

No Rust, no server, no API, no schema changes.

---

## Testing

Unit tests in `nlpQuery.test.ts` (vitest):

| Input | Mode | Expected query | Expected dateFrom | Expected dateTo |
|-------|------|---------------|------------------|-----------------|
| `"artifactory in the last two days"` | text | `"artifactory"` | now − 2d | now |
| `"artifactory and token last week"` | text | `"artifactory token"` | now − 7d | now |
| `"token since monday"` | text | `"token"` | last Mon | now |
| `"report before christmas"` | text | `"report"` | — | Dec 25 |
| `"between january and february"` | text | `""` → revert to raw | — | — |
| `"report between january and february"` | text | `"report"` | Jan 1 | Feb 28 |
| `"the quick brown fox"` | text | `"quick brown fox"` | — | — |
| `"\"the quick\" brown fox"` | text | `"\"the quick\" brown fox"` (quoted preserved) | — | — |
| `"foo AND bar"` | regex | `"foo AND bar"` (unchanged) | — | — |
| `"artifactory in the last two days"` | exact | `"artifactory in the last two days"` | now − 2d | now |

---

## Breaking Changes

None. The transform is transparent: the raw query is always preserved internally;
only the string sent to the API changes. Users who do not use natural language
date phrases see no difference in behaviour.

<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import IconChevronLeft from '$lib/icons/IconChevronLeft.svelte';
	import IconChevronRight from '$lib/icons/IconChevronRight.svelte';
	import type { SearchResult, ContextLine } from '$lib/api';
	import { getContext as fetchContext } from '$lib/api';
	import { highlightLine } from '$lib/highlight';
	import { contextWindow } from '$lib/settingsStore';

	/** All hits for this file, ordered by relevance (first hit is primary). */
	export let hits: SearchResult[];
	/** Current search query — used to highlight matches in the filename for path-only results. */
	export let query = '';

	$: result = hits[activeHitIndex] ?? hits[0];

	const dispatch = createEventDispatcher<{ open: SearchResult }>();

	let activeHitIndex = 0;
	let contextStart = 0;
	let contextMatchIndex: number | null = null;
	let contextLines: ContextLine[] = [];
	let contextLoaded = false;
	let el: HTMLElement;

	onMount(() => {
		let timer: ReturnType<typeof setTimeout> | null = null;

		const observer = new IntersectionObserver(
			(entries) => {
				if (entries[0].isIntersecting) {
					timer = setTimeout(() => {
						observer.disconnect();
						loadContext();
					}, 1000);
				} else {
					if (timer !== null) {
						clearTimeout(timer);
						timer = null;
					}
				}
			},
			{ rootMargin: '200px' }
		);
		observer.observe(el);
		return () => {
			observer.disconnect();
			if (timer !== null) clearTimeout(timer);
		};
	});

	async function loadContext() {
		const hit = hits[activeHitIndex] ?? hits[0];
		// Skip context loading for path matches (line 0) and metadata matches (line 1).
		// LINE_CONTENT_START = 2: line 0 = file path, line 1 = metadata, line 2+ = content.
		if (hit.line_number < 2) {
			contextLoaded = true;
			return;
		}
		try {
			const resp = await fetchContext(
				hit.source,
				hit.path,
				hit.line_number,
				$contextWindow,
				hit.archive_path ?? undefined
			);
			contextStart = resp.start;
			contextMatchIndex = resp.match_index;
			const lines = resp.lines;
			if (lines.length > 0) {
				const highlighted = await Promise.all(lines.map(l => highlightLine(l.content, hit.path)));
				contextLines = lines;
				highlightedContextLines = highlighted;
			} else {
				contextLines = lines;
				highlightedSnippet = await highlightLine(hit.snippet, hit.path);
			}
		} catch {
			// silently fall back to snippet
			try { highlightedSnippet = await highlightLine(hit.snippet, hit.path); } catch { /* ignore */ }
		} finally {
			contextLoaded = true;
		}
	}

	function switchToHit(i: number) {
		if (i === activeHitIndex) return;
		activeHitIndex = i;
		contextLoaded = false;
		contextLines = [];
		highlightedContextLines = [];
		highlightedSnippet = '';
		contextStart = 0;
		contextMatchIndex = null;
		loadContext();
	}

	function openFile() {
		dispatch('open', hits[activeHitIndex] ?? hits[0]);
	}

	function formatSize(bytes: number | null): string {
		if (bytes === null) return '';
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
		return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
	}

	function formatDate(timestamp: number): string {
		return new Date(timestamp * 1000).toLocaleString();
	}

	function openAlias(alias: string) {
		const i = alias.indexOf('::');
		dispatch('open', {
			...result,
			path: i >= 0 ? alias.slice(0, i) : alias,
			archive_path: i >= 0 ? alias.slice(i + 2) : null,
		});
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' || e.key === ' ') openFile();
	}

	function displayPath(r: SearchResult): string {
		return r.archive_path ? `${r.path}::${r.archive_path}` : r.path;
	}

	function fileName(r: SearchResult): string {
		const full = displayPath(r);
		const slash = full.lastIndexOf('/');
		const sep = full.lastIndexOf('::');
		const cut = Math.max(slash, sep);
		return cut >= 0 ? full.slice(cut + (full[cut] === ':' ? 2 : 1)) : full;
	}

	/** Convert raw line_number to user-visible display number. */
	function displayLine(n: number): number {
		// LINE_CONTENT_START = 2: display line = server line - 1 (1-based content index).
		return n >= 2 ? n - 1 : n;
	}

	/** True if this is a metadata match (line 1: path=0, metadata=1, content=2+). */
	function isMetadataMatch(r: SearchResult): boolean {
		return r.line_number === 1;
	}

	/** True if this is a path/filename match. */
	function isPathMatch(r: SearchResult): boolean {
		return r.line_number === 0;
	}

	/** True if this is a content match (has a navigable line number). */
	function isContentMatch(r: SearchResult): boolean {
		return r.line_number >= 2;
	}

	function escapeHtml(s: string): string {
		return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
	}

	/** Return HTML with query terms wrapped in <mark> for filename-only results. */
	function highlightPath(path: string, q: string): string {
		// Split on whitespace and any non-alphanumeric/non-underscore character,
		// mirroring the backend's build_fts_query tokenisation so that e.g.
		// "img.jpg" highlights both "img" and "jpg" in the filename.
		const terms = q.trim().split(/[\s\W]+/).filter(t => t.length >= 3);
		if (terms.length === 0) return escapeHtml(path);
		const pattern = new RegExp(
			terms.map(t => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'),
			'gi'
		);
		let out = '';
		let last = 0;
		let m: RegExpExecArray | null;
		while ((m = pattern.exec(path)) !== null) {
			out += escapeHtml(path.slice(last, m.index));
			out += `<mark class="path-match">${escapeHtml(m[0])}</mark>`;
			last = m.index + m[0].length;
		}
		out += escapeHtml(path.slice(last));
		return out;
	}

	let aliasesExpanded = false;

	/** Highlighted HTML for context lines (set after loadContext resolves). */
	let highlightedContextLines: string[] = [];
	/** Highlighted HTML for the snippet fallback (set after loadContext resolves). */
	let highlightedSnippet = '';
</script>

<article class="result" bind:this={el}>
	<!-- svelte-ignore a11y-no-static-element-interactions -->
	<div
		class="result-header"
		on:click={openFile}
		on:keydown={handleKeydown}
		role="button"
		tabindex="0"
		title={isContentMatch(result) ? `Open file at line ${displayLine(result.line_number)}` : 'Open file'}
	>
		<div class="result-row1">
			<span class="badge">{result.source}</span>
			<span class="file-path" title={displayPath(result)}>
				<span class="path-desktop">
					{#if isPathMatch(result)}
						{@html highlightPath(displayPath(result), query)}
					{:else}
						{displayPath(result)}
					{/if}
				</span>
				<span class="path-mobile-name">
					{#if isPathMatch(result)}
						{@html highlightPath(fileName(result), query)}
					{:else}
						{fileName(result)}
					{/if}
				</span>
			</span>
			{#if hits.length === 1 && isContentMatch(hits[0])}
				<span class="line-ref">:{displayLine(hits[0].line_number)}</span>
			{:else if hits.length > 1}
				<!-- svelte-ignore a11y-no-static-element-interactions -->
				<!-- svelte-ignore a11y-click-events-have-key-events -->
				<span class="hit-nav" title="{activeHitIndex + 1} of {hits.length}{hits[0].hits_truncated ? '+' : ''} hits" on:click|stopPropagation>
					<button class="hit-nav-btn" class:hit-nav-hidden={activeHitIndex === 0} on:click|stopPropagation={() => switchToHit(activeHitIndex - 1)} title="Previous hit (line {displayLine(hits[activeHitIndex - 1]?.line_number ?? 0)})">
						<IconChevronLeft />
					</button>
					<span class="line-ref nav-line-ref">:{displayLine(hits[activeHitIndex].line_number)}</span>
					<button class="hit-nav-btn" class:hit-nav-hidden={activeHitIndex >= hits.length - 1} on:click|stopPropagation={() => switchToHit(activeHitIndex + 1)} title="Next hit (line {displayLine(hits[activeHitIndex + 1]?.line_number ?? 0)})">
						<IconChevronRight />
					</button>
					{#if hits[0].hits_truncated}
						<span class="truncated-badge" title="More than {hits.length} matches in this file">+</span>
					{/if}
				</span>
			{/if}
		</div>
		<div class="result-row2">
			{#if result.duplicate_paths && result.duplicate_paths.length > 0}
				<!-- svelte-ignore a11y-click-events-have-key-events -->
				<span
					class="alias-badge"
					title={aliasesExpanded ? 'Hide duplicate paths' : 'Show duplicate paths'}
					on:click|stopPropagation={() => (aliasesExpanded = !aliasesExpanded)}
				>+{result.duplicate_paths.length} duplicate{result.duplicate_paths.length === 1 ? '' : 's'}</span>
			{/if}
			<div class="file-meta">
			{#if result.kind && result.kind !== 'raw'}
				<span class="meta-kind" title="File type">{result.kind}</span>
			{/if}
			{#if result.size !== null && result.size !== undefined}
				<span class="meta-item" title="File size">{formatSize(result.size)}</span>
			{/if}
			<span class="meta-item" title="Last modified">{formatDate(result.mtime)}</span>
		</div>
		</div><!-- result-row2 -->
	</div><!-- result-header -->
	{#if aliasesExpanded && result.duplicate_paths && result.duplicate_paths.length > 0}
		<div class="aliases">
			{#each result.duplicate_paths as alias}
				<button class="alias-path" on:click|stopPropagation={() => openAlias(alias)}>{alias}</button>
			{/each}
		</div>
	{/if}

	<div class="context-lines">
		{#if isMetadataMatch(result) && result.snippet}
			<!-- Metadata match (EXIF, mime, etc.) — show the matched tag -->
			<div class="line match">
				<span class="arrow meta-arrow">▶</span>
				<code class="lc">{result.snippet}</code>
			</div>
		{:else if isPathMatch(result)}
			<!-- Path/filename match — path is already shown in the header, skip snippet -->
		{:else if contextLines.length > 0}
			{#each contextLines as line, i}
				{@const isMatch = i === contextMatchIndex}
				<div class="line" class:match={isMatch}>
					<span class="ln">{displayLine(line.line_number)}</span>
					<span class="arrow">{isMatch ? '▶' : ' '}</span>
					<code class="lc">{@html highlightedContextLines[i] ?? escapeHtml(line.content)}</code>
				</div>
			{/each}
		{:else if contextLoaded}
			<div class="line match">
				<span class="ln">{displayLine(result.line_number)}</span>
				<span class="arrow">▶</span>
				<code class="lc">{@html highlightedSnippet || escapeHtml(result.snippet)}</code>
			</div>
		{:else}
			{#each Array(2 * $contextWindow + 1) as _, i}
				<div class="placeholder" class:match={i === $contextWindow}>
					<span class="ln">{i === $contextWindow ? displayLine(result.line_number) : ''}</span>
					<span class="arrow">{i === $contextWindow ? '▶' : ' '}</span>
					<span class="placeholder-bar"></span>
				</div>
			{/each}
		{/if}
	</div>
</article>

<style>
	.result {
		border: 1px solid var(--border);
		border-radius: var(--radius);
		overflow: hidden;
		margin-bottom: 12px;
	}

	.result:hover {
		border-color: var(--accent-muted);
	}

	.result-header {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 12px;
		background: var(--bg-secondary);
		cursor: pointer;
		user-select: none;
	}

	/* On desktop the row wrappers are invisible — children flow as flex items */
	.result-row1 { display: contents; }
	.result-row2 { display: contents; }

	.result-header:hover {
		background: var(--bg-hover);
	}

	.badge {
		padding: 1px 8px;
		border-radius: 20px;
		background: var(--badge-bg);
		color: var(--badge-text);
		font-size: 11px;
		flex-shrink: 0;
	}

	.file-path {
		color: var(--accent);
		font-family: var(--font-mono);
		font-size: 12px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
		min-width: 0;
	}

	/* Desktop: show full path, hide mobile-only spans */
	.path-mobile-name { display: none; }

	@media (max-width: 768px) {
		/* Two-row header layout */
		.result-header {
			flex-direction: column;
			align-items: stretch;
			gap: 4px;
			padding: 8px 12px;
		}
		.result-row1 {
			display: flex;
			align-items: center;
			gap: 8px;
			min-width: 0;
		}
		.result-row2 {
			display: flex;
			align-items: center;
			gap: 6px;
			flex-wrap: wrap;
		}

		/* Row 1: show filename only, not full path */
		.path-desktop { display: none; }
		.path-mobile-name {
			display: block;
			font-size: 13px;
			font-weight: 500;
			overflow: hidden;
			text-overflow: ellipsis;
			white-space: nowrap;
		}
		.file-path { white-space: normal; }
	}

	.file-path :global(.path-match) {
		background: var(--match-line-bg, rgba(255, 200, 0, 0.2));
		color: var(--match-text, #e3b341);
		border-radius: 2px;
		font-style: normal;
	}

	.line-ref {
		color: var(--text-dim);
		font-family: var(--font-mono);
		font-size: 12px;
		flex-shrink: 0;
		cursor: default;
	}

	.hit-nav {
		display: inline-flex;
		align-items: center;
		flex-shrink: 0;
		border: 1px solid var(--border);
		border-radius: 4px;
		overflow: hidden;
		height: 20px;
	}

	.nav-line-ref {
		padding: 0 5px;
		border-left: 1px solid var(--border);
		border-right: 1px solid var(--border);
		cursor: default;
	}

	.hit-nav-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 20px;
		height: 20px;
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		color: var(--text-dim);
		flex-shrink: 0;
		transition: color 0.1s, background-color 0.1s;
	}

	.hit-nav-btn:hover {
		color: var(--accent);
		background: var(--bg-hover);
	}

	.hit-nav-hidden {
		visibility: hidden;
		pointer-events: none;
	}

	.truncated-badge {
		padding: 0 4px;
		font-size: 11px;
		color: var(--text-dim);
		border-left: 1px solid var(--border);
		line-height: 20px;
		cursor: default;
	}


	.context-lines {
		background: var(--bg);
	}

	.line {
		display: flex;
		align-items: baseline;
		padding: 1px 0;
		overflow: hidden;
		min-width: 0;
	}

	.line.match {
		background: var(--match-line-bg);
		border-left: 2px solid var(--match-border);
	}

	.line:not(.match) {
		border-left: 2px solid transparent;
	}

	.ln {
		min-width: 48px;
		padding: 0 12px 0 8px;
		text-align: right;
		color: var(--text-dim);
		font-family: var(--font-mono);
		font-size: 12px;
		flex-shrink: 0;
		user-select: none;
	}

	.arrow {
		width: 14px;
		color: var(--accent);
		font-size: 10px;
		flex-shrink: 0;
		user-select: none;
	}

	.lc {
		padding: 0 12px 0 4px;
		white-space: pre;
		overflow: hidden;
		text-overflow: ellipsis;
		flex: 1;
		min-width: 0;
	}

	.meta-arrow {
		padding-left: 60px;
	}

	.placeholder {
		display: flex;
		align-items: center;
		min-height: 20px;
		padding: 1px 0;
		border-left: 2px solid transparent;
	}

	.placeholder.match {
		background: var(--match-line-bg);
		border-left-color: var(--match-border);
	}

	.placeholder-bar {
		flex: 1;
		height: 10px;
		margin: 0 12px 0 4px;
		border-radius: 3px;
		background: var(--border);
		opacity: 0.5;
	}

	.file-meta {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 8px;
		flex-shrink: 0;
	}

	.meta-kind {
		padding: 1px 6px;
		border-radius: 10px;
		background: var(--bg);
		border: 1px solid var(--border);
		color: var(--text-dim);
		font-size: 10px;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.meta-item {
		color: var(--text-dim);
		font-size: 11px;
		white-space: nowrap;
	}

	.alias-badge {
		padding: 1px 7px;
		border-radius: 20px;
		background: var(--bg);
		border: 1px solid var(--border);
		color: var(--text-dim);
		font-size: 11px;
		flex-shrink: 0;
		cursor: pointer;
	}

	.alias-badge:hover {
		border-color: var(--accent-muted);
		color: var(--text);
	}

	.aliases {
		background: var(--bg-secondary);
		border-top: 1px solid var(--border);
		padding: 4px 12px 6px;
	}

	.alias-path {
		display: block;
		width: 100%;
		background: none;
		border: none;
		text-align: left;
		cursor: pointer;
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--text-dim);
		padding: 2px 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.alias-path:hover {
		color: var(--accent);
		text-decoration: underline;
	}
</style>

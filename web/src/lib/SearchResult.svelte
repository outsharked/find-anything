<script lang="ts">
	import { createEventDispatcher, onMount } from 'svelte';
	import type { SearchResult } from '$lib/api';
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
	let contextLines: string[] = [];
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
		if (hit.line_number === 0) {
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
			contextLines = resp.lines;
		} catch {
			// silently fall back to snippet
		} finally {
			contextLoaded = true;
		}
	}

	function switchToHit(i: number) {
		if (i === activeHitIndex) return;
		activeHitIndex = i;
		contextLoaded = false;
		contextLines = [];
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
</script>

<article class="result" bind:this={el}>
	<!-- svelte-ignore a11y-no-static-element-interactions -->
	<div
		class="result-header"
		on:click={openFile}
		on:keydown={handleKeydown}
		role="button"
		tabindex="0"
		title={result.line_number > 0 ? `Open file at line ${result.line_number}` : 'Open file'}
	>
		<span class="badge">{result.source}</span>
		<span class="file-path" title={displayPath(result)}>
			{#if result.line_number === 0 && !result.snippet.startsWith('[')}
				{@html highlightPath(displayPath(result), query)}
			{:else}
				{displayPath(result)}
			{/if}
		</span>
		{#if hits.length === 1 && hits[0].line_number > 0}
			<span class="line-ref">:{hits[0].line_number}</span>
		{:else if hits.length > 1}
			<!-- svelte-ignore a11y-no-static-element-interactions -->
			<!-- svelte-ignore a11y-click-events-have-key-events -->
			<span class="hit-nav" title="{activeHitIndex + 1} of {hits.length} hits" on:click|stopPropagation>
				<button class="hit-nav-btn" class:hit-nav-hidden={activeHitIndex === 0} on:click|stopPropagation={() => switchToHit(activeHitIndex - 1)} title="Previous hit (line {hits[activeHitIndex - 1]?.line_number})">
					<svg width="8" height="8" viewBox="0 0 8 8" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
						<polyline points="5.5,1.5 2.5,4 5.5,6.5"/>
					</svg>
				</button>
				<span class="line-ref nav-line-ref">:{hits[activeHitIndex].line_number}</span>
				<button class="hit-nav-btn" class:hit-nav-hidden={activeHitIndex >= hits.length - 1} on:click|stopPropagation={() => switchToHit(activeHitIndex + 1)} title="Next hit (line {hits[activeHitIndex + 1]?.line_number})">
					<svg width="8" height="8" viewBox="0 0 8 8" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
						<polyline points="2.5,1.5 5.5,4 2.5,6.5"/>
					</svg>
				</button>
			</span>
		{/if}
		{#if result.aliases && result.aliases.length > 0}
			<!-- svelte-ignore a11y-click-events-have-key-events -->
			<span
				class="alias-badge"
				title={aliasesExpanded ? 'Hide duplicate paths' : 'Show duplicate paths'}
				on:click|stopPropagation={() => (aliasesExpanded = !aliasesExpanded)}
			>+{result.aliases.length} duplicate{result.aliases.length === 1 ? '' : 's'}</span>
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
	</div>
	{#if aliasesExpanded && result.aliases && result.aliases.length > 0}
		<div class="aliases">
			{#each result.aliases as alias}
				<button class="alias-path" on:click|stopPropagation={() => openAlias(alias)}>{alias}</button>
			{/each}
		</div>
	{/if}

	<div class="context-lines">
		{#if result.line_number === 0 && result.snippet.startsWith('[')}
			<!-- Metadata match (EXIF, mime, etc.) — show the matched tag -->
			<div class="line match">
				<span class="arrow meta-arrow">▶</span>
				<code class="lc">{result.snippet}</code>
			</div>
		{:else if result.line_number === 0}
			<!-- Path/filename match — path is already shown in the header, skip snippet -->
		{:else if contextLines.length > 0}
			{#each contextLines as content, i}
				{@const lineNum = contextStart + i}
				{@const isMatch = i === contextMatchIndex}
				<div class="line" class:match={isMatch}>
					<span class="ln">{lineNum}</span>
					<span class="arrow">{isMatch ? '▶' : ' '}</span>
					<code class="lc">{@html highlightLine(content, result.path)}</code>
				</div>
			{/each}
		{:else if contextLoaded}
			<div class="line match">
				<span class="ln">{result.line_number}</span>
				<span class="arrow">▶</span>
				<code class="lc">{@html highlightLine(result.snippet, result.path)}</code>
			</div>
		{:else}
			{#each Array(2 * $contextWindow + 1) as _, i}
				<div class="placeholder" class:match={i === $contextWindow}>
					<span class="ln">{i === $contextWindow ? result.line_number : ''}</span>
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

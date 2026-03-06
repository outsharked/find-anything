<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let source: string;
	export let path: string;
	export let archivePath: string | null = null;
	/** Effective resolved base URL (server value overridden by user profile). */
	export let baseUrl: string | null = null;

	const dispatch = createEventDispatcher<{
		back: void;
		navigate: { type: 'dir'; prefix: string } | { type: 'file'; path: string; kind: string };
	}>();

	type Segment = {
		label: string;
		separator: '/' | '::' | null; // separator BEFORE this segment (null for first)
		action: { type: 'dir'; prefix: string } | { type: 'file'; path: string; kind: string } | { type: 'current' };
	};

	$: segments = computeSegments(path, archivePath);

	$: externalHref = baseUrl
		? baseUrl.replace(/\/+$/, '') + '/' +
		  path.replace(/^\/+/, '').split('/').map(encodeURIComponent).join('/')
		: null;

	function computeSegments(outerPath: string, innerPath: string | null): Segment[] {
		const outerParts = outerPath.split('/');
		const result: Segment[] = [];

		for (let i = 0; i < outerParts.length; i++) {
			const cumulative = outerParts.slice(0, i + 1).join('/');
			const isLast = i === outerParts.length - 1;
			const sep: '/' | '::' | null = i === 0 ? null : '/';

			if (isLast && !innerPath) {
				result.push({ label: outerParts[i], separator: sep, action: { type: 'current' } });
			} else if (isLast && innerPath) {
				// Last outer segment is an archive — clicking opens its FileViewer
				result.push({ label: outerParts[i], separator: sep, action: { type: 'file', path: cumulative, kind: 'archive' } });
			} else {
				result.push({ label: outerParts[i], separator: sep, action: { type: 'dir', prefix: cumulative + '/' } });
			}
		}

		if (innerPath) {
			const innerParts = innerPath.split('/');
			for (let i = 0; i < innerParts.length; i++) {
				const cumulativeInner = innerParts.slice(0, i + 1).join('/');
				const isLast = i === innerParts.length - 1;
				const sep: '/' | '::' | null = i === 0 ? '::' : '/';

				if (isLast) {
					result.push({ label: innerParts[i], separator: sep, action: { type: 'current' } });
				} else {
					result.push({ label: innerParts[i], separator: sep, action: { type: 'dir', prefix: `${outerPath}::${cumulativeInner}/` } });
				}
			}
		}

		return result;
	}

	function handleSegmentClick(seg: Segment) {
		if (seg.action.type === 'current') return;
		dispatch('navigate', seg.action);
	}

	$: fullPath = archivePath ? `${path}::${archivePath}` : path;

	let copied = false;
	function copyPath() {
		const text = fullPath;
		if (navigator.clipboard) {
			navigator.clipboard.writeText(text).then(() => showCopied()).catch(() => fallbackCopy(text));
		} else {
			fallbackCopy(text);
		}
	}
	function fallbackCopy(text: string) {
		const ta = document.createElement('textarea');
		ta.value = text;
		ta.style.cssText = 'position:fixed;opacity:0';
		document.body.appendChild(ta);
		ta.focus();
		ta.select();
		try { document.execCommand('copy'); showCopied(); } finally { document.body.removeChild(ta); }
	}
	function showCopied() {
		copied = true;
		setTimeout(() => (copied = false), 2000);
	}
</script>

<div class="path-bar">
	<button class="back-btn" on:click={() => dispatch('back')}>← results</button>
	<button class="badge" on:click={() => dispatch('navigate', { type: 'dir', prefix: '' })}>{source}</button>
	<span class="path-plain">
		{#each segments as seg}
			{#if seg.separator}<span class="sep">{seg.separator}</span>{/if}
			{#if seg.action.type === 'current'}
				<span class="seg seg--current">{seg.label}</span>
			{:else}
				<button class="seg seg--link" on:click={() => handleSegmentClick(seg)}>{seg.label}</button>
			{/if}
		{/each}
		<button class="copy-btn" class:copied on:click={copyPath} title={copied ? '' : 'Copy path'}>
			{#if copied}
				<svg width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
					<polyline points="2,7 5,10 11,3" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>
				</svg>
				<span class="copied-label">Copied</span>
			{:else}
				<svg width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
					<rect x="4" y="1" width="8" height="9" rx="1.5" stroke="currentColor" stroke-width="1.3"/>
					<path d="M2 4H1.5A1.5 1.5 0 0 0 0 5.5v6A1.5 1.5 0 0 0 1.5 13H8A1.5 1.5 0 0 0 9.5 11.5V11" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
				</svg>
			{/if}
		</button>
	</span>
	{#if externalHref}
		<a class="external-link" href={externalHref} target="_blank" rel="noopener noreferrer" title="Open in file manager">↗</a>
	{/if}
</div>

<style>
	.path-bar {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 8px 16px;
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
		min-height: 38px;
		overflow: hidden;
	}

	.back-btn {
		background: none;
		border: 1px solid var(--border);
		color: var(--text-muted);
		padding: 3px 10px;
		border-radius: var(--radius);
		font-size: 12px;
		flex-shrink: 0;
		cursor: pointer;
	}

	.back-btn:hover {
		border-color: var(--accent);
		color: var(--accent);
	}

	.badge {
		padding: 1px 8px;
		border-radius: 20px;
		background: var(--badge-bg);
		color: var(--badge-text);
		font-size: 11px;
		flex-shrink: 0;
		border: none;
		cursor: pointer;
	}

	.badge:hover {
		opacity: 0.75;
	}

	.path-plain {
		font-family: var(--font-mono);
		font-size: 12px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
		min-width: 0;
		display: flex;
		align-items: baseline;
		gap: 0;
		color: var(--accent);
	}

	.sep {
		color: var(--text-dim);
		padding: 0 1px;
		user-select: none;
	}

	.seg {
		font-family: var(--font-mono);
		font-size: 12px;
		white-space: nowrap;
	}

	.seg--current {
		color: var(--accent);
	}

	.seg--link {
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		color: var(--text-muted);
	}

	.seg--link:hover {
		color: var(--accent);
		text-decoration: underline;
	}

	.copy-btn {
		background: none;
		border: none;
		padding: 2px 4px;
		cursor: pointer;
		color: var(--text-dim);
		flex-shrink: 0;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		border-radius: 3px;
		transition: color 0.15s;
		vertical-align: middle;
	}

	.copy-btn:hover {
		color: var(--accent);
	}

	.copy-btn.copied {
		color: #3fb950;
	}

	.copied-label {
		font-family: var(--font-mono);
		font-size: 11px;
		white-space: nowrap;
	}

	.external-link {
		margin-left: 4px;
		color: var(--text-dim);
		text-decoration: none;
		font-size: 11px;
		flex-shrink: 0;
	}

	.external-link:hover {
		color: var(--accent);
	}
</style>

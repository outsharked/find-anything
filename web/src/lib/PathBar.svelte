<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import IconCopy from '$lib/icons/IconCopy.svelte';
	import IconCheck from '$lib/icons/IconCheck.svelte';
	export let source: string;
	export let path: string;
	export let archivePath: string | null = null;
	/** Whether to show the ← results button. False when the user deeplinked directly. */
	export let showBack = true;
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
			navigator.clipboard.writeText(text).then(() => showCopied()).catch(() => fallbackCopy(text, showCopied));
		} else {
			fallbackCopy(text, showCopied);
		}
	}
	function fallbackCopy(text: string, done: () => void) {
		const ta = document.createElement('textarea');
		ta.value = text;
		ta.style.cssText = 'position:fixed;opacity:0';
		document.body.appendChild(ta);
		ta.focus();
		ta.select();
		try { document.execCommand('copy'); done(); } finally { document.body.removeChild(ta); }
	}
	function showCopied() {
		copied = true;
		setTimeout(() => (copied = false), 2000);
	}

</script>

<div class="path-bar">
	<div class="path-controls">
		{#if showBack}
		<button class="back-btn" on:click={() => dispatch('back')}>← results</button>
		{/if}
		<button class="badge" on:click={() => dispatch('navigate', { type: 'dir', prefix: '' })}>{source}</button>
	</div>
	<span class="path-plain">
		{#each segments as seg}
			{#if seg.separator}<span class="sep">{seg.separator}</span>{/if}
			{#if seg.action.type === 'current'}
				<span class="seg seg--current">{seg.label}</span>
			{:else}
				<button class="seg seg--link" on:click={() => handleSegmentClick(seg)}>{seg.label}</button>
			{/if}
		{/each}
		<button class="copy-btn" class:copied on:click={copyPath} data-tooltip="Copy path">
			{#if copied}
				<IconCheck />
				<span class="copied-label">Copied</span>
			{:else}
				<IconCopy />
			{/if}
		</button>
	</span>
</div>

<style>
	.path-bar {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 10px;
		padding: 8px 16px;
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
		min-height: 38px;
	}

	.path-controls {
		display: flex;
		align-items: center;
		gap: 6px;
		flex-shrink: 0;
	}

	.back-btn {
		background: var(--badge-bg);
		border: none;
		color: var(--badge-text);
		padding: 3px 10px;
		border-radius: 20px;
		font-size: 12px;
		flex-shrink: 0;
		cursor: pointer;
	}

	.back-btn:hover {
		opacity: 0.75;
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
		flex: 1;
		min-width: 0;
		display: flex;
		flex-wrap: wrap;
		align-items: center;
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
		word-break: break-all;
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
		margin-left: 6px;
		cursor: pointer;
		color: var(--text-dim);
		flex-shrink: 0;
		display: inline-flex;
		align-self: center;
		align-items: center;
		gap: 4px;
		border-radius: 3px;
		transition: color 0.15s;
		position: relative;
	}

	.copy-btn[data-tooltip]:not(.copied)::after {
		content: attr(data-tooltip);
		position: absolute;
		top: calc(100% + 4px);
		left: 50%;
		transform: translateX(-50%);
		white-space: nowrap;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		color: var(--text-muted);
		padding: 2px 6px;
		border-radius: 3px;
		font-size: 11px;
		opacity: 0;
		pointer-events: none;
		transition: opacity 0.1s;
		z-index: 100;
	}

	.copy-btn[data-tooltip]:not(.copied):hover::after {
		opacity: 1;
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

	@media (max-width: 768px) {
		.path-bar { padding: 6px 12px; gap: 4px 6px; }
		.path-plain { font-size: 11px; flex: 1 1 100%; }
	}

</style>

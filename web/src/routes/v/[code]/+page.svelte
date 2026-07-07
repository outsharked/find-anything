<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/stores';
	import { resolveLink } from '$lib/api';
	import type { ResolveLinkResponse } from '$lib/api';
	import DirectHeader from '$lib/DirectHeader.svelte';
	import DirectImageViewer from '$lib/DirectImageViewer.svelte';

	let viewState: 'loading' | 'found' | 'expired' | 'not-found' | 'error' = $state('loading');
	let link = $state<ResolveLinkResponse | null>(null);
	let textLines: string[] = $state([]);

	let code = $derived($page.params.code ?? '');

	let compositePath = $derived(link
		? link.archive_path
			? `${link.path}::${link.archive_path}`
			: link.path
		: '');

	function rawUrl(extra = '') {
		if (!link) return '';
		const u = new URL('/api/v1/raw', location.origin);
		u.searchParams.set('link_code', code);
		u.searchParams.set('source', link.source);
		u.searchParams.set('path', compositePath);
		return u.toString() + extra;
	}

	async function loadTextContent() {
		if (!link) return;
		const u = new URL('/api/v1/file', location.origin);
		u.searchParams.set('link_code', code);
		u.searchParams.set('source', link.source);
		u.searchParams.set('path', link.path);
		if (link.archive_path) u.searchParams.set('archive_path', link.archive_path);
		const resp = await fetch(u.toString());
		if (resp.ok) {
			const data = await resp.json();
			textLines = data.lines ?? [];
		}
	}

	onMount(async () => {
		try {
			const result = await resolveLink(code);
			if (result === null) {
				viewState = 'not-found';
			} else if (result === 'expired') {
				viewState = 'expired';
			} else {
				link = result;
				viewState = 'found';
				if (!isMediaKind(link.kind)) {
					await loadTextContent();
				}
			}
		} catch {
			viewState = 'error';
		}
	});

	function isMediaKind(kind: string) {
		return kind === 'image' || kind === 'pdf' || kind === 'video';
	}
</script>

<svelte:head>
	<title>{link ? link.filename : 'find-anything'}</title>
</svelte:head>

<div class="page">
	{#if viewState === 'loading'}
		<div class="center-msg">Loading…</div>
	{:else if viewState === 'not-found'}
		<div class="mini-header"><a href="/">find-anything</a></div>
		<div class="center-msg">Link not found</div>
	{:else if viewState === 'expired'}
		<div class="mini-header"><a href="/">find-anything</a></div>
		<div class="center-msg">This link has expired</div>
	{:else if viewState === 'error'}
		<div class="mini-header"><a href="/">find-anything</a></div>
		<div class="center-msg">Something went wrong</div>
	{:else if viewState === 'found' && link}
		<DirectHeader
			filename={link.filename}
			mtime={link.mtime}
			linkCode={code}
			source={link.source}
			path={link.path}
			archivePath={link.archive_path}
		/>

		<div class="viewer">
			{#if link.kind === 'image'}
				<DirectImageViewer src={rawUrl()} />
			{:else if link.kind === 'pdf'}
				<iframe src={rawUrl()} title={link.filename} class="embed-frame"></iframe>
			{:else if link.kind === 'video'}
				<!-- svelte-ignore a11y_media_has_caption -->
				<video controls src={rawUrl()} class="video-player">
					Your browser does not support the video element.
				</video>
			{:else}
				<pre class="text-content">{textLines.join('\n')}</pre>
			{/if}
		</div>
	{/if}
</div>

<style>
	:global(body) {
		margin: 0;
		padding: 0;
		background: var(--bg-primary, #13131f);
		color: var(--text, #cdd6f4);
		font-family: var(--font-sans, system-ui, sans-serif);
		height: 100vh;
		overflow: hidden;
	}

	.page {
		display: flex;
		flex-direction: column;
		height: 100vh;
		overflow: hidden;
	}

	.mini-header {
		padding: 10px 20px;
		background: var(--bg-secondary, #1a1a2e);
		border-bottom: 1px solid var(--border, #333);
	}

	.mini-header a {
		font-size: 14px;
		font-weight: 600;
		color: var(--accent, #7aa2f7);
		text-decoration: none;
	}

	.center-msg {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 16px;
		color: var(--text-muted, #888);
	}

	.viewer {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
		overflow: hidden;
	}

	.embed-frame {
		flex: 1;
		border: none;
		width: 100%;
		height: 100%;
	}

	.video-player {
		flex: 1;
		max-width: 100%;
		max-height: 100%;
		margin: auto;
		display: block;
	}

	.text-content {
		flex: 1;
		margin: 0;
		padding: 20px;
		overflow: auto;
		font-family: var(--font-mono, monospace);
		font-size: 13px;
		line-height: 1.6;
		white-space: pre-wrap;
		word-break: break-word;
		color: var(--text, #cdd6f4);
	}
</style>

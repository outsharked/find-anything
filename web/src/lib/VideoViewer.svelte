<script lang="ts">
	import { parseMetaTags } from '$lib/metaTags';
	import MetaDrawer from '$lib/MetaDrawer.svelte';

	export let src: string;
	/** Extracted metadata lines (line_number === 1, starting with '['). */
	export let metaLines: { content: string }[] = [];

	$: hasMeta = metaLines.length > 0;

	let noVideoTrack = false;
	let mediaError: string | null = null;

	// Extract container format from metadata (e.g. "[VIDEO:format] mkv" → "MKV").
	$: videoFormat = (() => {
		for (const m of metaLines) {
			const match = m.content.match(/\[VIDEO:format\]\s*(\S+)/i);
			if (match) return match[1].toUpperCase();
		}
		return null;
	})();

	function onLoadedMetadata(e: Event) {
		const video = e.target as HTMLVideoElement;
		noVideoTrack = video.videoWidth === 0;
	}

	function onError(e: Event) {
		const video = e.target as HTMLVideoElement;
		const err = video.error;
		if (!err) return;
		const messages: Record<number, string> = {
			1: 'Playback aborted.',
			2: 'Network error while loading.',
			3: 'Failed to decode — the video codec is not supported by your browser.',
			4: 'Format not supported by your browser.',
		};
		mediaError = messages[err.code] ?? `Media error (code ${err.code}).`;
	}
</script>

<div class="video-split-panel">
	<div class="video-split-left">
		{#if noVideoTrack}
			<div class="codec-warning">
				<strong>No video picture</strong> — the video codec{#if videoFormat} in this {videoFormat} file{/if}
				may not be supported on your computer. Try opening the file in VLC or another media player.
			</div>
		{/if}
		{#if mediaError}
			<div class="codec-warning">{mediaError}</div>
		{/if}
		<!-- svelte-ignore a11y-media-has-caption -->
		<video controls {src} class="video-player" on:loadedmetadata={onLoadedMetadata} on:error={onError}>
			Your browser does not support the video tag.
		</video>
	</div>
	{#if hasMeta}
		<MetaDrawer initialOpen={false}>
			{#each metaLines as meta}
				{#each parseMetaTags(meta.content) as tag}
					<div class="meta-row">
						<span class="tag-label">[{tag.label}]</span>
						<span class="tag-value">{tag.value}</span>
					</div>
				{/each}
			{/each}
		</MetaDrawer>
	{/if}
</div>

<style>
	.video-split-panel {
		flex: 1;
		display: flex;
		flex-direction: row;
		overflow: hidden;
		min-height: 0;
	}

	.video-split-left {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		background: var(--bg);
		overflow: auto;
		padding: 16px;
		min-width: 0;
		gap: 12px;
	}

	.codec-warning {
		background: color-mix(in srgb, #f59e0b 15%, transparent);
		border: 1px solid #f59e0b;
		border-radius: 6px;
		padding: 10px 14px;
		font-size: 0.875rem;
		color: var(--text);
		max-width: 480px;
		line-height: 1.5;
	}


	.video-player {
		max-width: 100%;
		max-height: 100%;
		outline: none;
		border-radius: 4px;
	}

	.meta-row {
		padding: 2px 0;
		line-height: 1.6;
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}

	.tag-label {
		color: var(--text-dim);
		flex-shrink: 0;
	}

	.tag-value {
		color: var(--text-muted);
	}

	@media (max-width: 768px) {
		.video-split-panel {
			flex-direction: column;
			overflow: visible;
			flex: none;
		}
		.video-split-left {
			border-right: none;
			border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.1));
		}
	}
</style>

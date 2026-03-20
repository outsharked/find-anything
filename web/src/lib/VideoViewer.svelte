<script lang="ts">
	import { parseMetaTags } from '$lib/metaTags';
	import MetaDrawer from '$lib/MetaDrawer.svelte';

	export let src: string;
	/** Extracted metadata lines (line_number === 1, starting with '['). */
	export let metaLines: { content: string }[] = [];

	$: hasMeta = metaLines.length > 0;
</script>

<div class="video-split-panel">
	<div class="video-split-left">
		<!-- svelte-ignore a11y-media-has-caption -->
		<video controls {src} class="video-player">
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
		align-items: center;
		justify-content: center;
		background: var(--bg);
		overflow: auto;
		padding: 16px;
		min-width: 0;
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


</style>

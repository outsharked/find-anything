<script lang="ts">
	import { parseMetaTags } from '$lib/metaTags';
	import MetaDrawer from '$lib/MetaDrawer.svelte';

	/** URL to stream the audio file. */
	export let src: string;
	/** Extracted metadata lines (line_number === 1, starting with '['). */
	export let metaLines: { content: string }[] = [];
</script>

<div class="audio-split-panel">
	<div class="audio-split-left">
		<!-- svelte-ignore a11y-media-has-caption -->
		<audio controls {src} class="audio-player">
			Your browser does not support the audio element.
		</audio>
	</div>
	<MetaDrawer initialOpen={true}>
		{#if metaLines.length > 0}
			{#each metaLines as meta}
				{#each parseMetaTags(meta.content) as tag}
					<div class="meta-row">
						<span class="tag-label">[{tag.label}]</span>
						<span class="tag-value">{tag.value}</span>
					</div>
				{/each}
			{/each}
		{:else}
			<div class="no-content">No metadata available.</div>
		{/if}
	</MetaDrawer>
</div>

<style>
	.audio-split-panel {
		flex: 1;
		display: flex;
		flex-direction: row;
		overflow: hidden;
		min-height: 0;
	}

	.audio-split-left {
		flex: 1;
		overflow: auto;
		display: flex;
		align-items: center;
		justify-content: center;
		border-right: 1px solid var(--border, rgba(255, 255, 255, 0.1));
		padding: 32px 16px;
		min-width: 0;
	}

	.audio-player {
		width: 100%;
		max-width: 480px;
		outline: none;
	}

	.no-content {
		padding: 24px;
		color: var(--text-dim);
		font-size: 13px;
		text-align: center;
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
		.audio-split-panel {
			flex-direction: column;
			overflow: visible;
			flex: none;
		}
		.audio-split-left {
			border-right: none;
			border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.1));
		}
	}
</style>

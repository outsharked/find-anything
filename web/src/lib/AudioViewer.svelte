<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { parseMetaTags } from '$lib/metaTags';

	/** URL to stream the audio file. */
	export let src: string;
	/** Extracted metadata lines (line_number === 1, starting with '['). */
	export let metaLines: { content: string }[] = [];
	/** Paths of duplicate / canonical copies (dedup aliases). */
	export let duplicatePaths: string[] = [];

	const dispatch = createEventDispatcher<{ openDuplicate: { path: string } }>();
</script>

<div class="audio-split-panel">
	<div class="audio-split-left">
		<!-- svelte-ignore a11y-media-has-caption -->
		<audio controls {src} class="audio-player">
			Your browser does not support the audio element.
		</audio>
	</div>
	<div class="audio-split-right">
		{#if metaLines.length > 0 || duplicatePaths.length > 0}
			{#each duplicatePaths as dup}
				<div class="meta-row duplicate-row">
					<span class="duplicate-label">DUPLICATE:</span>
					<button class="duplicate-link" on:click={() => dispatch('openDuplicate', { path: dup })}>{dup}</button>
				</div>
			{/each}
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
	</div>
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

	.audio-split-right {
		width: 300px;
		flex-shrink: 0;
		overflow-y: auto;
		padding: 12px 16px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		background: var(--bg-secondary);
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

	.duplicate-row {
		display: flex;
		align-items: baseline;
		gap: 6px;
	}

	.duplicate-label {
		flex-shrink: 0;
		color: var(--accent, #58a6ff);
		font-weight: 600;
	}

	.duplicate-link {
		background: none;
		border: none;
		padding: 0;
		font-family: inherit;
		font-size: inherit;
		color: var(--accent, #58a6ff);
		cursor: pointer;
		text-align: left;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.duplicate-link:hover {
		text-decoration: underline;
	}
</style>

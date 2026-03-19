<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { parseMetaTags } from '$lib/metaTags';

	/** URL of the image (or converted PNG for unsupported formats). */
	export let src: string;
	/** File path — used as the img alt text. */
	export let path: string;
	/** When true, render full-width; when false, render split view with metadata. */
	export let fullWidth: boolean = false;
	/** CSS inline style for the aspect-ratio loading placeholder. */
	export let placeholderStyle: string = '';
	/** Extracted metadata lines (line_number === 0, starting with '['). */
	export let metaLines: { content: string }[] = [];
	/** Paths of duplicate / canonical copies (dedup aliases). */
	export let duplicatePaths: string[] = [];

	const dispatch = createEventDispatcher<{ openDuplicate: { path: string } }>();

	let imageLoaded = false;
	let imageError = false;

	// Reset load state whenever the image URL changes.
	$: { src; imageLoaded = false; imageError = false; }
</script>

{#if fullWidth}
	<!-- Full-width scrollable image -->
	<div class="image-full-panel">
		{#if imageError}
			<div class="img-placeholder img-placeholder--error" style={placeholderStyle}>Image unavailable</div>
		{:else}
			{#if !imageLoaded}<div class="img-placeholder img-placeholder--loading" style={placeholderStyle}></div>{/if}
			<img {src} alt={path}
				class="image-full" class:img-hidden={!imageLoaded}
				on:load={() => imageLoaded = true}
				on:error={() => imageError = true} />
		{/if}
	</div>
{:else}
	<!-- Split view: image left, metadata right -->
	<div class="image-split-panel">
		<div class="image-split-left">
			{#if imageError}
				<div class="img-placeholder img-placeholder--error" style={placeholderStyle}>Image unavailable</div>
			{:else}
				{#if !imageLoaded}<div class="img-placeholder img-placeholder--loading" style={placeholderStyle}></div>{/if}
				<img {src} alt={path}
					class="image-split-img" class:img-hidden={!imageLoaded}
					on:load={() => imageLoaded = true}
					on:error={() => imageError = true} />
			{/if}
		</div>
		<div class="image-split-right">
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
{/if}

<style>
	/* Image split view */
	.image-split-panel {
		flex: 1;
		display: flex;
		flex-direction: row;
		overflow: hidden;
		min-height: 0;
	}

	.image-split-left {
		flex: 1;
		overflow: auto;
		display: flex;
		align-items: center;
		justify-content: center;
		border-right: 1px solid var(--border, rgba(255, 255, 255, 0.1));
		padding: 16px;
		min-width: 0;
	}

	.image-split-img {
		max-width: 100%;
		max-height: 100%;
		object-fit: contain;
	}

	.image-split-right {
		width: 300px;
		flex-shrink: 0;
		overflow-y: auto;
		padding: 12px 16px;
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--text-muted);
		background: var(--bg-secondary);
	}

	/* Image full-width view */
	.image-full-panel {
		flex: 1;
		overflow: auto;
		background: var(--bg);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding: 16px;
	}

	.image-full {
		max-width: 100%;
		height: auto;
		display: block;
	}

	/* Loading / error placeholder */
	.img-placeholder {
		width: 100%;
		min-height: 200px;
		display: flex;
		align-items: center;
		justify-content: center;
		border-radius: 4px;
		font-size: 12px;
		color: var(--text-dim);
	}

	.img-placeholder--loading {
		background: var(--bg-hover, rgba(255, 255, 255, 0.04));
		animation: img-pulse 1.4s ease-in-out infinite;
	}

	.img-placeholder--error {
		background: var(--bg-hover, rgba(255, 255, 255, 0.04));
		color: var(--text-muted);
	}

	@keyframes img-pulse {
		0%, 100% { opacity: 0.5; }
		50%       { opacity: 1;   }
	}

	.img-hidden {
		display: none;
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

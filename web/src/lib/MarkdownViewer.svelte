<script lang="ts">
	/** Pre-rendered HTML string from marked.parse(). */
	export let rendered: string;
</script>

<div class="markdown-content">
	{@html rendered}
</div>

<style>
	/* Markdown rendering styles — child selectors use :global() because the
	   content is injected via {@html} and Svelte can't see those elements. */
	.markdown-content {
		padding: 32px 48px;
		max-width: 900px;
		margin: 0 auto;
		color: var(--text);
		line-height: 1.7;
	}

	.markdown-content :global(h1),
	.markdown-content :global(h2),
	.markdown-content :global(h3) {
		border-bottom: 1px solid var(--border);
		padding-bottom: 0.4em;
		margin-top: 32px;
		margin-bottom: 20px;
		font-weight: 600;
	}

	.markdown-content :global(h1) {
		font-size: 2em;
		margin-top: 0;
	}

	.markdown-content :global(h2) {
		font-size: 1.5em;
	}

	.markdown-content :global(h3) {
		font-size: 1.25em;
	}

	.markdown-content :global(h4) {
		font-size: 1.1em;
		font-weight: 600;
		margin-top: 24px;
		margin-bottom: 16px;
	}

	.markdown-content :global(h5),
	.markdown-content :global(h6) {
		font-size: 1em;
		font-weight: 600;
		margin-top: 20px;
		margin-bottom: 12px;
	}

	.markdown-content :global(a) {
		color: var(--accent);
		text-decoration: none;
	}

	.markdown-content :global(a:hover) {
		text-decoration: underline;
	}

	.markdown-content :global(code) {
		background: var(--bg-secondary);
		padding: 0.2em 0.4em;
		border-radius: 3px;
		font-family: var(--font-mono);
		font-size: 0.9em;
	}

	.markdown-content :global(pre) {
		background: var(--bg-secondary);
		padding: 16px;
		border-radius: 6px;
		overflow-x: auto;
		margin: 20px 0;
		line-height: 1.5;
	}

	.markdown-content :global(pre code) {
		background: none;
		padding: 0;
	}

	.markdown-content :global(blockquote) {
		border-left: 4px solid var(--accent);
		padding: 8px 0 8px 20px;
		margin: 24px 0;
		color: var(--text-muted);
	}

	.markdown-content :global(table) {
		border-collapse: collapse;
		width: 100%;
		margin: 24px 0;
	}

	.markdown-content :global(th),
	.markdown-content :global(td) {
		border: 1px solid var(--border);
		padding: 8px 12px;
		text-align: left;
	}

	.markdown-content :global(th) {
		background: var(--bg-secondary);
		font-weight: 600;
	}

	.markdown-content :global(tr:nth-child(even)) {
		background: var(--bg-hover);
	}

	.markdown-content :global(img) {
		max-width: 100%;
		height: auto;
	}

	.markdown-content :global(ul),
	.markdown-content :global(ol) {
		padding-left: 2em;
		margin: 16px 0;
	}

	.markdown-content :global(li) {
		margin: 6px 0;
		line-height: 1.6;
	}

	.markdown-content :global(li > p) {
		margin: 4px 0;
	}

	.markdown-content :global(p) {
		margin: 16px 0;
	}

	.markdown-content :global(p:first-child) {
		margin-top: 0;
	}

	.markdown-content :global(hr) {
		border: none;
		border-top: 1px solid var(--border);
		margin: 32px 0;
	}
</style>

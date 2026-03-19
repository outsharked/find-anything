<script lang="ts">
	let showHelp = false;

	function toggleHelp() { showHelp = !showHelp; }
	function closeHelp() { showHelp = false; }
</script>

<div class="help-wrap">
	<button
		class="help-btn"
		class:active={showHelp}
		title="Search syntax help"
		on:click={toggleHelp}
		aria-label="Search syntax help"
	>?</button>
	{#if showHelp}
		<!-- svelte-ignore a11y-no-static-element-interactions -->
		<!-- svelte-ignore a11y-click-events-have-key-events -->
		<div class="help-backdrop" on:click={closeHelp}></div>
		<div class="help-popup" role="dialog" aria-label="Search syntax help">
			<div class="help-section">
				<div class="help-heading">Scope prefixes</div>
				<div class="help-row"><code>file:</code><span>Search filenames only</span></div>
				<div class="help-row"><code>doc:</code><span>Search whole document (all matching lines)</span></div>
				<div class="help-row"><em>(none)</em><span>Search individual lines (default)</span></div>
			</div>
			<div class="help-section">
				<div class="help-heading">Match prefixes</div>
				<div class="help-row"><code>exact:</code><span>Exact substring match</span></div>
				<div class="help-row"><code>regex:</code><span>Regular expression</span></div>
				<div class="help-row"><em>(none)</em><span>Fuzzy / ranked match (default)</span></div>
			</div>
			<div class="help-section">
				<div class="help-heading">Combining</div>
				<div class="help-row"><code>file:exact:</code><span>Exact filename match</span></div>
				<div class="help-row"><code>regex:doc:</code><span>Regex across whole document</span></div>
			</div>
			<div class="help-section">
				<div class="help-heading">File type</div>
				<div class="help-row"><code>type:pdf</code><span>PDF files</span></div>
				<div class="help-row"><code>type:text</code><span>Code &amp; text files</span></div>
				<div class="help-row"><code>type:image</code><span>Images</span></div>
				<div class="help-row"><code>type:audio</code><span>Audio</span></div>
				<div class="help-row"><code>type:video</code><span>Video</span></div>
				<div class="help-row"><code>type:document</code><span>Office / eBook</span></div>
				<div class="help-row"><code>type:archive</code><span>Archives (ZIP, RAR, …)</span></div>
			</div>
			<div class="help-section">
				<div class="help-heading">Natural language dates</div>
				<div class="help-row"><em>yesterday, last week, last month, …</em></div>
				<div class="help-desc">Date phrases in your query are detected automatically and filter results by file modification date.</div>
			</div>
			<div class="help-section">
				<div class="help-heading">Quotes</div>
				<div class="help-row"><em>"multi word phrase"</em></div>
				<div class="help-desc">Wrap phrases in quotes to match them as a unit.</div>
			</div>
		</div>
	{/if}
</div>

<style>
	.help-wrap {
		position: relative;
		flex-shrink: 0;
	}

	.help-btn {
		background: none;
		border: 1px solid var(--text-muted);
		cursor: pointer;
		color: var(--text-muted);
		font-size: 11px;
		font-weight: 700;
		width: 18px;
		height: 18px;
		border-radius: 50%;
		padding: 0;
		line-height: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
	}

	.help-btn:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
		color: var(--text);
		border-color: var(--text);
	}

	.help-btn.active {
		color: var(--accent, #58a6ff);
		border-color: var(--accent, #58a6ff);
	}

	.help-backdrop {
		position: fixed;
		inset: 0;
		z-index: 199;
	}

	.help-popup {
		position: absolute;
		top: calc(100% + 8px);
		left: 0;
		z-index: 200;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 6px;
		padding: 12px 16px;
		min-width: 300px;
		max-height: calc(100vh - 80px);
		overflow-y: auto;
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
		font-size: 13px;
	}

	.help-section {
		margin-bottom: 12px;
	}

	.help-section:last-child {
		margin-bottom: 0;
	}

	.help-heading {
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-muted);
		margin-bottom: 6px;
	}

	.help-row {
		display: flex;
		align-items: baseline;
		gap: 10px;
		padding: 2px 0;
		color: var(--text);
	}

	.help-row code {
		font-family: var(--font-mono);
		font-size: 12px;
		color: var(--accent, #58a6ff);
		white-space: nowrap;
		flex-shrink: 0;
		min-width: 110px;
	}

	.help-row em {
		font-style: normal;
		color: var(--text-muted);
		font-size: 12px;
	}

	.help-row span {
		color: var(--text-dim, var(--text-muted));
		font-size: 12px;
	}

	.help-desc {
		color: var(--text-muted);
		font-size: 12px;
		margin-top: 2px;
		line-height: 1.5;
	}
</style>

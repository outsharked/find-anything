<div class="help-sections">
	<div class="help-section">
		<div class="help-heading">Scope prefixes</div>
		<div class="help-row"><code>file:</code><span>Search filenames only</span></div>
		<div class="help-row"><code>doc:</code><span>Search whole document (all matching lines)</span></div>
		<div class="help-row"><code>source:</code><span>Limit to a specific source/path</span></div>
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
		<div class="help-row"><code>type:text</code><span>Text files</span></div>
		<div class="help-row"><code>type:code</code><span>Code files</span></div>
		<div class="help-row"><code>type:document</code><span>Office files</span></div>
		<div class="help-row"><code>type:epub</code><span>eBook files</span></div>
		<div class="help-row"><code>type:image</code><span>Images</span></div>
		<div class="help-row"><code>type:audio</code><span>Audio</span></div>
		<div class="help-row"><code>type:video</code><span>Video</span></div>
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

<style>
	.help-sections {
		padding: 12px 16px;
	}

	.help-section { margin-bottom: 12px; }
	.help-section:last-child { margin-bottom: 0; }

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

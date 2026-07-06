<script lang="ts">
	import { onMount } from 'svelte';
	import { getErrors, getStats } from '$lib/api';
	import type { IndexingError } from '$lib/api';

	let { onNavigate }: { onNavigate?: (source: string, path: string) => void } = $props();

	let sources: string[] = $state([]);
	let selectedSource = $state('');
	let errors: IndexingError[] = $state([]);
	let total = $state(0);
	let loading = $state(false);
	let loadError: string | null = $state(null);
	/** Track which rows have the error text expanded. */
	let expanded: Set<string> = $state(new Set());

	onMount(async () => {
		try {
			const stats = await getStats();
			sources = stats.sources.map((s) => s.name);
			if (sources.length > 0) {
				selectedSource = sources[0];
				await fetchErrors();
			}
		} catch (e) {
			loadError = String(e);
		}
	});

	async function fetchErrors() {
		if (!selectedSource) return;
		loading = true;
		loadError = null;
		expanded = new Set();
		try {
			const resp = await getErrors(selectedSource);
			errors = resp.errors;
			total = resp.total;
		} catch (e) {
			loadError = String(e);
		} finally {
			loading = false;
		}
	}

	function toggleExpand(path: string) {
		const next = new Set(expanded);
		if (next.has(path)) {
			next.delete(path);
		} else {
			next.add(path);
		}
		expanded = next;
	}

	function handlePathClick(err: IndexingError) {
		onNavigate?.(selectedSource, err.path);
	}

	function fmtRelativeTime(epochSecs: number): string {
		const diff = Math.floor(Date.now() / 1000) - epochSecs;
		if (diff < 60) return 'just now';
		if (diff < 3600) return Math.floor(diff / 60) + 'm ago';
		if (diff < 86400) return Math.floor(diff / 3600) + 'h ago';
		return Math.floor(diff / 86400) + 'd ago';
	}

	const ERROR_PREVIEW_LEN = 120;
</script>

{#if sources.length > 1}
	<div class="source-row">
		<label class="source-label" for="errors-source-select">Source</label>
		<select
			id="errors-source-select"
			class="source-select"
			bind:value={selectedSource}
			onchange={fetchErrors}
		>
			{#each sources as src (src)}
				<option value={src}>{src}</option>
			{/each}
		</select>
	</div>
{/if}

{#if loading}
	<div class="status">Loading…</div>
{:else if loadError}
	<div class="status error">{loadError}</div>
{:else if errors.length === 0}
	<div class="status empty">No indexing errors.</div>
{:else}
	<div class="summary">
		{total} error{total !== 1 ? 's' : ''} recorded
	</div>
	<table class="errors-table">
		<thead>
			<tr>
				<th class="col-path">Path</th>
				<th class="col-error">Error</th>
				<th class="col-seen">Last seen</th>
				<th class="col-count">Count</th>
			</tr>
		</thead>
		<tbody>
			{#each errors as err (err.path)}
				{@const isExpanded = expanded.has(err.path)}
				{@const needsTruncate = err.error.length > ERROR_PREVIEW_LEN}
				<tr class="error-row">
					<td class="col-path">
						<!-- svelte-ignore a11y_click_events_have_key_events -->
						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<span class="path-link" onclick={() => handlePathClick(err)} title={err.path}>
							{err.path}
						</span>
					</td>
					<td class="col-error">
						<span class="error-msg">
							{isExpanded ? err.error : err.error.slice(0, ERROR_PREVIEW_LEN)}
							{#if needsTruncate && !isExpanded}…{/if}
						</span>
						{#if needsTruncate}
							<!-- svelte-ignore a11y_click_events_have_key_events -->
							<!-- svelte-ignore a11y_no_static_element_interactions -->
							<span
								class="toggle-btn"
								onclick={() => toggleExpand(err.path)}
							>{isExpanded ? 'Show less' : 'Show more'}</span>
						{/if}
					</td>
					<td class="col-seen" title={new Date(err.last_seen * 1000).toLocaleString()}>
						{fmtRelativeTime(err.last_seen)}
					</td>
					<td class="col-count">{err.count}</td>
				</tr>
			{/each}
		</tbody>
	</table>
{/if}

<style>
	.source-row {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 16px;
	}

	.source-label {
		font-size: 12px;
		color: var(--text-muted);
		flex-shrink: 0;
	}

	.source-select {
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		color: var(--text);
		font-size: 13px;
		padding: 4px 8px;
		cursor: pointer;
	}

	.status {
		color: var(--text-muted);
		font-size: 13px;
		padding: 24px;
		text-align: center;
	}

	.status.error {
		color: #f85149;
	}

	.status.empty {
		color: var(--text-muted);
	}

	.summary {
		font-size: 12px;
		color: var(--text-muted);
		margin-bottom: 12px;
	}

	.errors-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 12px;
	}

	.errors-table th {
		text-align: left;
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
		padding: 4px 8px;
		border-bottom: 1px solid var(--border);
	}

	.error-row {
		border-bottom: 1px solid var(--border);
		vertical-align: top;
	}

	.error-row:last-child {
		border-bottom: none;
	}

	.error-row td {
		padding: 8px;
	}

	.col-path {
		width: 35%;
		max-width: 240px;
	}

	.col-error {
		width: 50%;
	}

	.col-seen {
		width: 10%;
		white-space: nowrap;
		color: var(--text-muted);
	}

	.col-count {
		width: 5%;
		text-align: right;
		color: var(--text-muted);
	}

	.path-link {
		color: var(--accent, #58a6ff);
		cursor: pointer;
		font-family: var(--font-mono);
		word-break: break-all;
		font-size: 11px;
	}

	.path-link:hover {
		text-decoration: underline;
	}

	.error-msg {
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 11px;
		word-break: break-word;
	}

	.toggle-btn {
		display: inline-block;
		margin-left: 6px;
		color: var(--accent, #58a6ff);
		cursor: pointer;
		font-size: 11px;
		white-space: nowrap;
	}

	.toggle-btn:hover {
		text-decoration: underline;
	}
</style>

<script lang="ts">
	import { onMount } from 'svelte';
	import { getInboxStatus, retryFailedInbox } from '$lib/api';
	import type { InboxStatusResponse } from '$lib/api';

	let inbox: InboxStatusResponse | null = $state(null);
	let loading = $state(true);
	let error: string | null = $state(null);
	let retrying = $state(false);
	let retryMessage: string | null = $state(null);

	onMount(() => {
		fetchStatus();
	});

	async function fetchStatus() {
		error = null;
		try {
			inbox = await getInboxStatus();
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	}

	async function handleRetry() {
		retrying = true;
		retryMessage = null;
		try {
			const result = await retryFailedInbox();
			retryMessage = `Queued ${result.retried} item${result.retried === 1 ? '' : 's'} for reprocessing.`;
			await fetchStatus();
		} catch (e) {
			error = String(e);
		} finally {
			retrying = false;
		}
	}
</script>

{#if loading}
	<p class="muted">Loading…</p>
{:else if error}
	<p class="error">{error}</p>
{:else if inbox}
	<div class="section">
		<h3 class="section-title">Inbox</h3>
		<div class="row">
			<span class="label">Pending</span>
			<span class="value">{inbox.pending.length}</span>
		</div>
		<div class="row">
			<span class="label">Failed</span>
			<span class="value" class:warn={inbox.failed.length > 0}>{inbox.failed.length}</span>
		</div>
		{#if inbox.failed.length > 0}
			<div class="actions">
				<button class="btn" onclick={handleRetry} disabled={retrying}>
					{retrying ? 'Retrying…' : 'Retry Failed'}
				</button>
			</div>
		{/if}
		{#if retryMessage}
			<p class="success">{retryMessage}</p>
		{/if}
	</div>
{/if}

<style>
	.section {
		display: flex;
		flex-direction: column;
		gap: 8px;
		max-width: 400px;
	}

	.section-title {
		font-size: 13px;
		font-weight: 600;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		margin: 0 0 4px;
	}

	.row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 6px 0;
		border-bottom: 1px solid var(--border);
		font-size: 13px;
	}

	.label {
		color: var(--text-muted);
	}

	.value {
		color: var(--text);
		font-variant-numeric: tabular-nums;
	}

	.value.warn {
		color: var(--accent-warn, #e3a140);
		font-weight: 600;
	}

	.actions {
		margin-top: 8px;
	}

	.btn {
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		color: var(--text);
		font-size: 13px;
		padding: 6px 14px;
		border-radius: 4px;
		cursor: pointer;
	}

	.btn:hover:not(:disabled) {
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
	}

	.btn:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.muted {
		color: var(--text-muted);
		font-size: 13px;
	}

	.error {
		color: var(--accent-error, #f85149);
		font-size: 13px;
	}

	.success {
		color: var(--accent-success, #3fb950);
		font-size: 13px;
	}
</style>

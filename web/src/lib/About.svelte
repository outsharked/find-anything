<script lang="ts">
	import { getSettings } from '$lib/api';
	import { onMount } from 'svelte';

	let serverVersion = '';
	let buildHash = '';
	let latestVersion = '';
	let checkState: 'idle' | 'checking' | 'up-to-date' | 'update-available' | 'error' = 'idle';

	onMount(async () => {
		try {
			const settings = await getSettings();
			serverVersion = settings.version;
			buildHash = settings.git_hash;
		} catch {
			serverVersion = '(unavailable)';
		}
	});

	async function checkForUpdates() {
		checkState = 'checking';
		try {
			const resp = await fetch('https://api.github.com/repos/jamietre/find-anything/releases/latest');
			if (!resp.ok) throw new Error(`GitHub API returned ${resp.status}`);
			const data = await resp.json();
			latestVersion = data.tag_name?.replace(/^v/, '') ?? '';
			checkState = latestVersion === serverVersion ? 'up-to-date' : 'update-available';
		} catch {
			checkState = 'error';
		}
	}
</script>

<div class="about">
	<div class="row">
		<span class="label">Server version</span>
		<span class="value">{serverVersion || '…'}{buildHash && buildHash !== 'unknown' ? ` (${buildHash})` : ''}</span>
	</div>

	<div class="update-row">
		<button class="check-btn" on:click={checkForUpdates} disabled={checkState === 'checking'}>
			{checkState === 'checking' ? 'Checking…' : 'Check for updates'}
		</button>

		{#if checkState === 'up-to-date'}
			<span class="status ok">Up to date</span>
		{:else if checkState === 'update-available'}
			<span class="status update">
				v{latestVersion} available —
				<a href="https://github.com/jamietre/find-anything/releases/latest" target="_blank" rel="noreferrer">
					release notes
				</a>
			</span>
		{:else if checkState === 'error'}
			<span class="status err">Could not reach GitHub</span>
		{/if}
	</div>
</div>

<style>
	.about {
		display: flex;
		flex-direction: column;
		gap: 20px;
	}

	.row {
		display: flex;
		align-items: baseline;
		gap: 12px;
	}

	.label {
		font-size: 13px;
		color: var(--text-muted);
		min-width: 120px;
	}

	.value {
		font-size: 13px;
		color: var(--text);
		font-family: monospace;
	}

	.update-row {
		display: flex;
		align-items: center;
		gap: 14px;
	}

	.check-btn {
		font-size: 13px;
		padding: 5px 12px;
		border-radius: 5px;
		border: 1px solid var(--border);
		background: var(--bg-secondary);
		color: var(--text);
		cursor: pointer;
	}

	.check-btn:hover:not(:disabled) {
		background: var(--bg-hover, rgba(255,255,255,0.08));
	}

	.check-btn:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.status {
		font-size: 13px;
	}

	.status.ok   { color: #3fb950; }
	.status.update { color: #f0883e; }
	.status.err  { color: var(--text-muted); }

	.status a {
		color: inherit;
	}
</style>

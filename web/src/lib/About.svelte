<script lang="ts">
	import { getSettings, getUpdateCheck, applyUpdate } from '$lib/api';
	import { onMount } from 'svelte';

	let serverVersion = $state('');
	let buildHash = $state('');

	type CheckState = 'idle' | 'checking' | 'up-to-date' | 'update-available' | 'no-systemd' | 'no-asset' | 'error';
	let checkState: CheckState = $state('idle');
	let latestVersion = $state('');
	let errorMsg = $state('');

	type ApplyState = 'idle' | 'applying' | 'restarting' | 'done' | 'error';
	let applyState: ApplyState = $state('idle');
	let applyMsg = $state('');

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
		errorMsg = '';
		try {
			const info = await getUpdateCheck();
			latestVersion = info.latest;

			if (!info.restart_supported) {
				checkState = info.restart_unsupported_reason?.includes('asset') ? 'no-asset' : 'no-systemd';
				errorMsg = info.restart_unsupported_reason ?? '';
			} else if (info.update_available) {
				checkState = 'update-available';
			} else {
				checkState = 'up-to-date';
			}
		} catch (e) {
			checkState = 'error';
			errorMsg = e instanceof Error ? e.message : String(e);
		}
	}

	async function doUpdate() {
		applyState = 'applying';
		applyMsg = '';
		try {
			const result = await applyUpdate();
			applyMsg = result.message;
			applyState = 'restarting';

			// Poll /api/v1/settings until the server responds again, then reload.
			await pollUntilBack();
			applyState = 'done';
			location.reload();
		} catch (e) {
			applyState = 'error';
			applyMsg = e instanceof Error ? e.message : String(e);
		}
	}

	async function pollUntilBack() {
		for (let i = 0; i < 60; i++) {
			await new Promise(r => setTimeout(r, 2000));
			try {
				const resp = await fetch('/api/v1/settings');
				if (resp.ok) return;
			} catch { /* server still down */ }
		}
	}
</script>

<div class="about">
	<div class="row">
		<span class="label">Server version</span>
		<span class="value">{serverVersion || '…'}{buildHash && buildHash !== 'unknown' ? ` (${buildHash})` : ''}</span>
	</div>

	<div class="update-row">
		{#if applyState === 'idle' || applyState === 'error'}
			<button class="check-btn" onclick={checkForUpdates} disabled={checkState === 'checking'}>
				{checkState === 'checking' ? 'Checking…' : 'Check for updates'}
			</button>
		{/if}

		{#if checkState === 'up-to-date'}
			<span class="status ok">Up to date (v{latestVersion})</span>
		{:else if checkState === 'update-available' && applyState === 'idle'}
			<span class="status update">v{latestVersion} available</span>
			<button class="apply-btn" onclick={doUpdate}>Update &amp; restart</button>
		{:else if checkState === 'update-available' && applyState === 'applying'}
			<span class="status update">Downloading v{latestVersion}…</span>
		{:else if applyState === 'restarting'}
			<span class="status update">Restarting… ({applyMsg})</span>
		{:else if applyState === 'error'}
			<span class="status err">{applyMsg}</span>
		{:else if checkState === 'no-systemd'}
			<span class="status muted">Self-update requires systemd</span>
		{:else if checkState === 'no-asset'}
			<span class="status muted">{errorMsg}</span>
		{:else if checkState === 'error'}
			<span class="status err">{errorMsg || 'Could not reach GitHub'}</span>
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

	.status.ok     { color: #3fb950; }
	.status.update { color: #f0883e; }
	.status.err    { color: #f85149; }
	.status.muted  { color: var(--text-muted); }

	.apply-btn {
		font-size: 13px;
		padding: 5px 12px;
		border-radius: 5px;
		border: 1px solid #f0883e;
		background: transparent;
		color: #f0883e;
		cursor: pointer;
	}

	.apply-btn:hover {
		background: rgba(240, 136, 62, 0.12);
	}
</style>

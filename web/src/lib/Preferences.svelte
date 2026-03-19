<script lang="ts">
	import { profile } from '$lib/profile';
	import { contextWindow, tabWidth } from '$lib/settingsStore';

	function setTheme(value: string) {
		profile.update((p) => ({ ...p, theme: value as 'dark' | 'light' | 'system' }));
	}

	function setContextWindow(value: number) {
		profile.update((p) => ({ ...p, contextWindow: value }));
		contextWindow.set(value);
	}

	function setTabWidth(value: number) {
		profile.update((p) => ({ ...p, tabWidth: value }));
		tabWidth.set(value);
	}
</script>

<div class="section-title">Appearance</div>
<div class="pref-row">
	<label class="pref-label" for="theme">Theme</label>
	<div class="pref-control">
		<select
			id="theme"
			class="select"
			value={$profile.theme ?? 'dark'}
			on:change={(e) => setTheme(e.currentTarget.value)}
		>
			<option value="dark">Dark</option>
			<option value="light">Light</option>
			<option value="system">Inherit from browser</option>
		</select>
	</div>
</div>

<div class="section-title" style="margin-top: 24px;">File viewer</div>
<div class="pref-row">
	<label class="pref-label" for="tab-width">Tab width</label>
	<div class="pref-control">
		<select
			id="tab-width"
			class="select"
			value={$profile.tabWidth ?? $tabWidth}
			on:change={(e) => setTabWidth(Number(e.currentTarget.value))}
		>
			<option value={1}>1</option>
			<option value={2}>2</option>
			<option value={4}>4</option>
			<option value={8}>8</option>
		</select>
		{#if $profile.tabWidth !== undefined}
			<button class="clear-btn" on:click={() => { profile.update(p => { const {tabWidth: _, ...rest} = p; return rest; }); tabWidth.set(4); }}>Reset</button>
		{/if}
	</div>
</div>

<div class="section-title" style="margin-top: 24px;">Search results</div>
<div class="pref-row">
	<label class="pref-label" for="ctx-window">Lines of context</label>
	<div class="pref-control">
		<select
			id="ctx-window"
			class="select"
			value={$profile.contextWindow ?? $contextWindow}
			on:change={(e) => setContextWindow(Number(e.currentTarget.value))}
		>
			<option value={0}>0 (match only)</option>
			<option value={1}>1 (±1 line)</option>
			<option value={2}>2 (±2 lines)</option>
			<option value={3}>3 (±3 lines)</option>
			<option value={5}>5 (±5 lines)</option>
		</select>
		{#if $profile.contextWindow !== undefined}
			<button class="clear-btn" on:click={() => { profile.update(p => { const {contextWindow: _, ...rest} = p; return rest; }); contextWindow.set(1); }}>Reset</button>
		{/if}
	</div>
</div>

<style>
	.section-title {
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-muted);
		margin-bottom: 12px;
	}

	.pref-row {
		display: flex;
		align-items: center;
		gap: 16px;
		margin-bottom: 16px;
	}

	.pref-label {
		font-size: 13px;
		color: var(--text);
		min-width: 140px;
	}

	.pref-control {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.select {
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		color: var(--text);
		font-size: 13px;
		padding: 5px 8px;
		outline: none;
		cursor: pointer;
	}

	.select:focus {
		border-color: var(--accent);
	}

	.clear-btn {
		background: none;
		border: 1px solid var(--border);
		color: var(--text-muted);
		font-size: 12px;
		padding: 4px 10px;
		border-radius: var(--radius);
		cursor: pointer;
		flex-shrink: 0;
	}

	.clear-btn:hover {
		border-color: #f85149;
		color: #f85149;
	}
</style>

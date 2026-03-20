<script lang="ts">
	import { onMount } from 'svelte';
	import { profile } from '$lib/profile';
	import { contextWindow, tabWidth } from '$lib/settingsStore';
	import { listSources } from '$lib/api';

	const HANDLER_INSTALL_CMD =
		'irm https://github.com/jamietre/find-anything/releases/latest/download/install-handler.ps1 | iex';
	let handlerCopied = false;
	function copyHandlerCmd() {
		navigator.clipboard.writeText(HANDLER_INSTALL_CMD).then(() => {
			handlerCopied = true;
			setTimeout(() => (handlerCopied = false), 2000);
		});
	}

	function setHandlerInstalled(value: boolean) {
		profile.update((p) => ({ ...p, handlerInstalled: value || undefined }));
	}

	let sourceNames: string[] = [];

	onMount(async () => {
		try {
			const sources = await listSources();
			sourceNames = sources.map((s) => s.name);
		} catch { /* server may not be reachable in all contexts */ }
	});

	function setSourceRoot(name: string, root: string) {
		profile.update((p) => ({
			...p,
			sourceRoots: { ...(p.sourceRoots ?? {}), [name]: root.trim() }
		}));
	}

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

<div class="section-title" style="margin-top: 24px;">Open in Explorer</div>
{#if !$profile.handlerInstalled}
<div class="section-desc">
	Install the <code>findanything://</code> protocol handler on Windows to enable the
	"Open in Explorer" button in the file viewer. Run in PowerShell (no admin required):
</div>
<div class="handler-row">
	<code class="handler-cmd">{HANDLER_INSTALL_CMD}</code>
	<button class="copy-btn" on:click={copyHandlerCmd}>
		{handlerCopied ? 'Copied!' : 'Copy'}
	</button>
</div>
{/if}
<div class="pref-row" style="margin-top: 12px;">
	<label class="pref-label" for="handler-installed">Handler installed</label>
	<div class="pref-control">
		<input
			id="handler-installed"
			type="checkbox"
			checked={!!$profile.handlerInstalled}
			on:change={(e) => setHandlerInstalled(e.currentTarget.checked)}
		/>
		{#if !$profile.handlerInstalled}
			<span class="handler-hint">Check this after installing to enable the "Open in Explorer" button</span>
		{/if}
	</div>
</div>

{#if sourceNames.length > 0}
<div class="section-title" style="margin-top: 24px;">Source roots</div>
<div class="section-desc">Local path for each source, used by "Open in Explorer" in the file viewer.</div>
{#each sourceNames as name}
<div class="pref-row">
	<label class="pref-label" for="root-{name}">{name}</label>
	<div class="pref-control">
		<input
			id="root-{name}"
			type="text"
			class="path-input"
			placeholder="C:\Share or /mnt/nas"
			value={$profile.sourceRoots?.[name] ?? ''}
			on:change={(e) => setSourceRoot(name, e.currentTarget.value)}
		/>
	</div>
</div>
{/each}
{/if}

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

	.section-desc {
		font-size: 12px;
		color: var(--text-dim);
		margin-bottom: 12px;
	}

	.path-input {
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		color: var(--text);
		font-size: 13px;
		font-family: var(--font-mono);
		padding: 5px 8px;
		width: 320px;
		outline: none;
	}

	.path-input:focus {
		border-color: var(--accent);
	}

	.path-input::placeholder {
		color: var(--text-dim);
	}

	.handler-row {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-bottom: 4px;
	}

	.handler-cmd {
		font-family: var(--font-mono);
		font-size: 12px;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		padding: 6px 10px;
		color: var(--text);
		flex: 1;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		user-select: all;
	}

	.copy-btn {
		background: none;
		border: 1px solid var(--border);
		color: var(--text-muted);
		font-size: 12px;
		padding: 5px 12px;
		border-radius: var(--radius);
		cursor: pointer;
		flex-shrink: 0;
	}

	.copy-btn:hover {
		border-color: var(--accent);
		color: var(--accent);
	}

	.handler-hint {
		font-size: 12px;
		color: var(--text-dim);
	}
</style>

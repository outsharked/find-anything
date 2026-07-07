<script lang="ts">
	import { clickOutside } from '$lib/clickOutside';
	import IconBack from '$lib/icons/IconBack.svelte';
	import IconFilter from '$lib/icons/IconFilter.svelte';
	import { KIND_GROUPS, KIND_OPTIONS } from '$lib/kindOptions';
	import type { SearchScope, SearchMatchType } from '$lib/searchPrefixes';
	import MobilePanel from '$lib/MobilePanel.svelte';

	type ChangeDetail = { sources: string[]; kinds: string[]; dateFrom?: number; dateTo?: number; caseSensitive: boolean; scope: SearchScope; matchType: SearchMatchType };

	let {
		sources = [],
		selectedSources = [],
		selectedKinds = [],
		dateFrom = '',
		dateTo = '',
		caseSensitive = false,
		scope = 'line',
		matchType = 'fuzzy',
		onChange
	}: {
		/** All available source names. */
		sources?: string[];
		/** Currently active sources (empty = all). */
		selectedSources?: string[];
		/** Currently active kind filter (empty = all). */
		selectedKinds?: string[];
		/** Current date-from value as ISO string (YYYY-MM-DD), or empty. */
		dateFrom?: string;
		/** Current date-to value as ISO string (YYYY-MM-DD), or empty. */
		dateTo?: string;
		/** Whether case-sensitive matching is active. */
		caseSensitive?: boolean;
		/** Current scope selection. */
		scope?: SearchScope;
		/** Current match type selection. */
		matchType?: SearchMatchType;
		onChange?: (detail: ChangeDetail) => void;
	} = $props();

	let isOpen = $state(false);

	// Draft state — what the user is currently editing inside the panel.
	let draftSources: string[] = $state([]);
	let draftKinds: string[] = $state([]);
	let draftFrom = $state('');
	let draftTo = $state('');
	let draftCaseSensitive = $state(false);
	let draftScope: SearchScope = $state('line');
	let draftMatchType: SearchMatchType = $state('fuzzy');

	// Sync draft from props whenever the panel opens.
	function openPanel() {
		draftSources = [...selectedSources];
		draftKinds = [...selectedKinds];
		draftFrom = dateFrom;
		draftTo = dateTo;
		draftCaseSensitive = caseSensitive;
		draftScope = scope;
		draftMatchType = matchType;
		isOpen = true;
	}

	function isoToUnix(iso: string): number | undefined {
		if (!iso) return undefined;
		const ms = Date.parse(iso + 'T00:00:00Z');
		return isNaN(ms) ? undefined : Math.floor(ms / 1000);
	}

	function apply() {
		onChange?.({
			sources: draftSources,
			kinds: draftKinds,
			dateFrom: isoToUnix(draftFrom),
			dateTo: isoToUnix(draftTo),
			caseSensitive: draftCaseSensitive,
			scope: draftScope,
			matchType: draftMatchType,
		});
		isOpen = false;
	}

	function clearAll() {
		draftSources = [];
		draftKinds = [];
		draftFrom = '';
		draftTo = '';
		draftCaseSensitive = false;
		draftScope = 'line';
		draftMatchType = 'fuzzy';
		onChange?.({ sources: [], kinds: [], caseSensitive: false, scope: 'line', matchType: 'fuzzy' });
		isOpen = false;
	}

	function toggleDraftSource(source: string) {
		if (draftSources.includes(source)) {
			draftSources = draftSources.filter((s) => s !== source);
		} else {
			draftSources = [...draftSources, source];
		}
	}

	function toggleDraftKind(kind: string) {
		if (draftKinds.includes(kind)) {
			draftKinds = draftKinds.filter((k) => k !== kind);
		} else {
			draftKinds = [...draftKinds, kind];
		}
	}

	// Whether the draft differs from what's currently applied (props).
	let isDirty = $derived(
		JSON.stringify(draftSources.slice().sort()) !== JSON.stringify(selectedSources.slice().sort()) ||
		JSON.stringify(draftKinds.slice().sort()) !== JSON.stringify(selectedKinds.slice().sort()) ||
		draftFrom !== dateFrom ||
		draftTo !== dateTo ||
		draftCaseSensitive !== caseSensitive ||
		draftScope !== scope ||
		draftMatchType !== matchType
	);

	let sourceFiltered = $derived(selectedSources.length > 0 && selectedSources.length < sources.length);
	let kindFiltered = $derived(selectedKinds.length > 0);
	let dateFiltered = $derived(dateFrom !== '' || dateTo !== '');
	let scopeActive = $derived(scope !== 'line');
	let matchActive = $derived(matchType !== 'fuzzy');
	let anyFilter = $derived(sourceFiltered || kindFiltered || dateFiltered || caseSensitive || scopeActive || matchActive);

	// Count badge: number of active filter dimensions
	let filterCount = $derived((sourceFiltered ? 1 : 0) + (kindFiltered ? 1 : 0) + (dateFiltered ? 1 : 0) + (caseSensitive ? 1 : 0) + (scopeActive ? 1 : 0) + (matchActive ? 1 : 0));

	function showFromPicker() {
		(document.getElementById('adv-date-from') as HTMLInputElement)?.showPicker();
	}
	function showToPicker() {
		(document.getElementById('adv-date-to') as HTMLInputElement)?.showPicker();
	}

	const SCOPE_OPTIONS: { value: SearchScope; label: string }[] = [
		{ value: 'line', label: 'Single-line' },
		{ value: 'file', label: 'Filename' },
		{ value: 'doc',  label: 'Document' },
	];
	const MATCH_OPTIONS: { value: SearchMatchType; label: string }[] = [
		{ value: 'fuzzy', label: 'Fuzzy' },
		{ value: 'exact', label: 'Exact' },
		{ value: 'regex', label: 'Regex' },
	];
</script>

<div class="advanced-search" use:clickOutside={() => (isOpen = false)}>
	<button
		class="trigger"
		class:active={anyFilter}
		onclick={() => (isOpen ? (isOpen = false) : openPanel())}
		title="Advanced filters"
	>
		<IconFilter />
		<span class="text">Advanced</span>
		{#if anyFilter}
			<span class="badge">{filterCount}</span>
		{/if}
		<span class="chevron" class:open={isOpen}>▾</span>
	</button>

	{#if isOpen}
		<div class="panel">
			<div class="panel-mobile-header">
				<button class="panel-back" onclick={() => (isOpen = false)} aria-label="Close filters">
					<IconBack />
				</button>
				<span class="panel-mobile-title">Filters</span>
			</div>
			<div class="panel-body">
				{#if sources.length > 0}
					<div class="section">
						<div class="section-header">
							<span class="section-title">Sources</span>
							{#if draftSources.length > 0 && draftSources.length < sources.length}
								<button class="clear-link" onclick={() => (draftSources = [])}>All</button>
							{/if}
						</div>
						<div class="source-list">
							{#each sources as source}
								<label class="source-item">
									<input
										type="checkbox"
										checked={draftSources.includes(source)}
										onchange={() => toggleDraftSource(source)}
									/>
									<span class="source-name">{source}</span>
								</label>
							{/each}
						</div>
					</div>
				{/if}

				<div class="section">
					<div class="section-header">
						{#if draftKinds.length > 0}
							<button class="clear-link" onclick={() => (draftKinds = [])}>All</button>
						{/if}
					</div>
					{#each KIND_GROUPS as group}
					{#if group.label}
						<div class="kind-group-label">{group.label}</div>
					{/if}
					<div class="kind-grid">
						{#each group.kinds as opt}
							<label class="kind-item">
								<input
									type="checkbox"
									checked={draftKinds.includes(opt.value)}
									onchange={() => toggleDraftKind(opt.value)}
								/>
								<span class="kind-label">{opt.label}</span>
							</label>
						{/each}
					</div>
				{/each}
				</div>

				<div class="section">
					<div class="section-header">
						<span class="section-title">Date range</span>
						{#if draftFrom || draftTo}
							<button class="clear-link" onclick={() => { draftFrom = ''; draftTo = ''; }}>Clear</button>
						{/if}
					</div>
					<div class="date-row">
						<label class="date-label" for="adv-date-from">From</label>
						<div class="date-wrap">
							<input
								id="adv-date-from"
								class="date-input"
								class:no-value={!draftFrom}
								type="date"
								bind:value={draftFrom}
							/>
							<button class="cal-btn" tabindex="-1" onclick={showFromPicker}>📅</button>
						</div>
					</div>
					<div class="date-row">
						<label class="date-label" for="adv-date-to">To</label>
						<div class="date-wrap">
							<input
								id="adv-date-to"
								class="date-input"
								class:no-value={!draftTo}
								type="date"
								bind:value={draftTo}
							/>
							<button class="cal-btn" tabindex="-1" onclick={showToPicker}>📅</button>
						</div>
					</div>
				</div>

				<div class="section">
					<div class="section-header">
						<span class="section-title">Scope</span>
					</div>
					<div class="toggle-group">
						{#each SCOPE_OPTIONS as opt}
							<button
								class="toggle-btn"
								class:active={draftScope === opt.value}
								onclick={() => (draftScope = opt.value)}
								type="button"
							>{opt.label}</button>
						{/each}
					</div>
				</div>

				<div class="section">
					<div class="section-header">
						<span class="section-title">Match type</span>
					</div>
					<div class="toggle-group">
						{#each MATCH_OPTIONS as opt}
							<button
								class="toggle-btn"
								class:active={draftMatchType === opt.value}
								onclick={() => (draftMatchType = opt.value)}
								type="button"
							>{opt.label}</button>
						{/each}
					</div>
				</div>

				<div class="section">
					<label class="option-item">
						<input type="checkbox" bind:checked={draftCaseSensitive} />
						<span class="option-label">Case sensitive</span>
					</label>
				</div>
			</div>

			<div class="footer">
				{#if anyFilter}
					<button class="clear-all" onclick={clearAll}>Clear all</button>
				{/if}
				<button class="apply-btn" class:dirty={isDirty} disabled={!isDirty} onclick={apply}>Apply</button>
			</div>
		</div>
	{/if}
</div>

<style>
	.advanced-search {
		position: relative;
		display: inline-block;
	}

	.trigger {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 5px 10px;
		border: 1px solid var(--border);
		border-radius: 6px;
		background: var(--bg);
		color: var(--text);
		font-size: 13px;
		cursor: pointer;
		transition: all 0.15s;
	}

	.trigger:hover {
		border-color: var(--accent);
		background: var(--hover-bg);
	}

	.trigger.active {
		border-color: var(--accent);
		background: var(--chip-active);
		color: #fff;
	}

	.trigger :global(svg) {
		display: block;
		flex-shrink: 0;
	}

	.text {
		white-space: nowrap;
	}

	.badge {
		background: rgba(255, 255, 255, 0.3);
		border-radius: 10px;
		padding: 1px 6px;
		font-size: 11px;
		font-weight: 600;
	}

	.chevron {
		font-size: 10px;
		transition: transform 0.2s;
		opacity: 0.7;
	}

	.chevron.open {
		transform: rotate(180deg);
	}

	.panel {
		position: absolute;
		top: calc(100% + 4px);
		right: 0;
		min-width: 240px;
		max-height: calc(100vh - 80px);
		overflow: hidden;
		display: flex;
		flex-direction: column;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 6px;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
		z-index: 1000;
	}

	.panel-body {
		flex: 1;
		overflow-y: auto;
		overflow-x: hidden;
		min-height: 0;
		scrollbar-width: thin;
		scrollbar-color: var(--border) transparent;
	}

	.panel-body::-webkit-scrollbar {
		width: 6px;
	}

	.panel-body::-webkit-scrollbar-track {
		background: transparent;
	}

	.panel-body::-webkit-scrollbar-thumb {
		background: var(--border);
		border-radius: 3px;
	}

	.section {
		padding: 10px 12px;
		border-bottom: 1px solid var(--border);
	}

	.section-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 6px;
	}

	.section-title {
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}

	.clear-link {
		background: none;
		border: none;
		color: var(--accent);
		font-size: 12px;
		cursor: pointer;
		padding: 0;
	}

	.clear-link:hover {
		text-decoration: underline;
	}

	.source-list {
		max-height: 200px;
		overflow-y: auto;
	}

	.source-item {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 4px 0;
		cursor: pointer;
	}

	.source-item input[type='checkbox'] {
		cursor: pointer;
		margin: 0;
	}

	.source-name {
		font-size: 13px;
		color: var(--text);
	}

	.kind-group-label {
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted, #888);
		margin: 8px 0 2px;
	}

	.kind-group-label:first-child {
		margin-top: 0;
	}

	.kind-grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 2px 8px;
		margin-bottom: 4px;
	}

	.kind-item {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 3px 0;
		cursor: pointer;
	}

	.kind-item input[type='checkbox'] {
		cursor: pointer;
		margin: 0;
	}

	.kind-label {
		font-size: 13px;
		color: var(--text);
	}

	.date-row {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-top: 6px;
	}

	.date-label {
		font-size: 12px;
		color: var(--text-muted);
		width: 28px;
		flex-shrink: 0;
	}

	.date-wrap {
		flex: 1;
		display: flex;
		align-items: center;
		border: 1px solid var(--border);
		border-radius: 4px;
		background: var(--bg);
	}

	.date-wrap:focus-within {
		border-color: var(--accent);
	}

	.date-input {
		flex: 1;
		padding: 4px 6px;
		border: none;
		background: transparent;
		color: var(--text);
		font-size: 12px;
		font-family: inherit;
		/* hide the browser's built-in calendar icon */
		&::-webkit-calendar-picker-indicator { display: none; }
	}

	/* dim the placeholder format text when no value is set */
	.date-input.no-value::-webkit-datetime-edit {
		opacity: 0.25;
	}

	.date-input:focus {
		outline: none;
	}

	.cal-btn {
		background: none;
		border: none;
		border-left: 1px solid var(--border);
		padding: 2px 6px;
		cursor: pointer;
		font-size: 13px;
		line-height: 1;
		color: var(--text-muted);
		flex-shrink: 0;
	}

	.cal-btn:hover {
		color: var(--text);
		background: var(--hover-bg);
	}

	.option-item {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 2px 0;
		cursor: pointer;
	}

	.option-item input[type='checkbox'] {
		cursor: pointer;
		margin: 0;
	}

	.option-label {
		font-size: 13px;
		color: var(--text);
	}

	.footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 8px 12px;
		background: var(--hover-bg);
		border-top: 1px solid var(--border);
		flex-shrink: 0;
	}

	.clear-all {
		background: none;
		border: none;
		color: var(--text-muted);
		font-size: 12px;
		cursor: pointer;
		padding: 0;
	}

	.clear-all:hover {
		color: var(--text);
		text-decoration: underline;
	}

	.apply-btn {
		margin-left: auto;
		padding: 4px 14px;
		border-radius: 4px;
		border: 1px solid var(--border);
		background: none;
		color: var(--text-muted);
		font-size: 12px;
		font-weight: 600;
		cursor: default;
		transition: all 0.15s;
	}

	.apply-btn.dirty {
		border-color: var(--accent);
		background: var(--accent);
		color: #fff;
		cursor: pointer;
	}

	.apply-btn.dirty:hover {
		opacity: 0.85;
	}

	.toggle-group {
		display: flex;
		gap: 4px;
	}

	.toggle-btn {
		flex: 1;
		padding: 4px 8px;
		border: 1px solid var(--border);
		border-radius: 4px;
		background: none;
		color: var(--text-muted);
		font-size: 12px;
		cursor: pointer;
		transition: all 0.15s;
		white-space: nowrap;
	}

	.toggle-btn:hover {
		border-color: var(--accent);
		color: var(--text);
	}

	.toggle-btn.active {
		border-color: var(--accent);
		background: var(--accent);
		color: #fff;
	}

	.panel-mobile-header { display: none; }

	@media (max-width: 768px) {
		.trigger { padding: 5px 8px; gap: 4px; }
		.text { display: none; }
		.chevron { display: none; }

		/* Full-screen modal on mobile */
		.advanced-search { position: static; }
		.panel {
			position: fixed;
			top: 0;
			right: 0;
			bottom: 0;
			left: 0;
			min-width: 0;
			max-height: none;
			border-radius: 0;
			border: none;
			z-index: 2000;
		}
		.panel-mobile-header {
			display: flex;
			align-items: center;
			gap: 12px;
			padding: 12px 16px;
			border-bottom: 1px solid var(--border);
			background: var(--bg-secondary);
			flex-shrink: 0;
		}
		.panel-mobile-title {
			font-size: 16px;
			font-weight: 600;
			color: var(--text);
		}
		.panel-back {
			background: none;
			border: none;
			color: var(--text);
			cursor: pointer;
			padding: 4px;
			display: flex;
			align-items: center;
			justify-content: center;
			border-radius: 4px;
			min-width: 32px;
			min-height: 32px;
		}
		.panel-back:hover { background: var(--bg-hover); }
	}
</style>

<script lang="ts">
	import { clickOutside } from '$lib/clickOutside';

	let {
		sources = [],
		selected = [],
		onChange
	}: {
		/** All available source names. */
		sources?: string[];
		/** Currently active sources (empty = all) — controlled by the caller, not mutated locally. */
		selected?: string[];
		onChange?: (selected: string[]) => void;
	} = $props();

	let isOpen = $state(false);

	function toggle(source: string) {
		const next = selected.includes(source)
			? selected.filter((s) => s !== source)
			: [...selected, source];
		onChange?.(next);
	}

	function selectAll() {
		onChange?.([]);
		isOpen = false;
	}

	function handleClickOutside() {
		isOpen = false;
	}

	// Compute display text for the button
	let buttonText = $derived.by(() => {
		if (selected.length === 0) return 'All sources';
		if (selected.length === 1) return selected[0];
		return `${selected.length} of ${sources.length} sources`;
	});

	// Badge showing number of filtered sources
	let hasFilter = $derived(selected.length > 0 && selected.length < sources.length);
</script>

<div class="source-selector" use:clickOutside={handleClickOutside}>
	<button
		class="trigger"
		class:has-filter={hasFilter}
		onclick={() => (isOpen = !isOpen)}
		title="Filter by source"
	>
		<span class="icon">📁</span>
		<span class="text">{buttonText}</span>
		{#if hasFilter}
			<span class="badge">{selected.length}</span>
		{/if}
		<span class="chevron" class:open={isOpen}>▾</span>
	</button>

	{#if isOpen}
		<div class="dropdown">
			<div class="dropdown-header">
				<button class="action-btn" onclick={selectAll}>All sources</button>
			</div>
			<div class="dropdown-list">
				{#each sources as source}
					<label class="source-item">
						<input
							type="checkbox"
							checked={selected.includes(source)}
							onchange={() => toggle(source)}
						/>
						<span class="source-name">{source}</span>
					</label>
				{/each}
			</div>
		</div>
	{/if}
</div>

<style>
	.source-selector {
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
		min-width: 140px;
	}

	.trigger:hover {
		border-color: var(--accent);
		background: var(--hover-bg);
	}

	.trigger.has-filter {
		border-color: var(--accent);
		background: var(--chip-active);
		color: #fff;
	}

	.icon {
		font-size: 14px;
	}

	.text {
		flex: 1;
		text-align: left;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
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

	.dropdown {
		position: absolute;
		top: calc(100% + 4px);
		left: 0;
		min-width: 200px;
		max-width: 300px;
		background: var(--bg);
		border: 1px solid var(--border);
		border-radius: 6px;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
		z-index: 1000;
		overflow: hidden;
	}

	.dropdown-header {
		padding: 8px;
		border-bottom: 1px solid var(--border);
		background: var(--hover-bg);
	}

	.action-btn {
		width: 100%;
		padding: 4px 8px;
		border: 1px solid var(--border);
		border-radius: 4px;
		background: var(--bg);
		color: var(--text);
		font-size: 12px;
		cursor: pointer;
		transition: all 0.15s;
	}

	.action-btn:hover {
		border-color: var(--accent);
		background: var(--hover-bg);
	}

	.dropdown-list {
		max-height: 400px;
		overflow-y: auto;
	}

	.source-item {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 12px;
		cursor: pointer;
		transition: background 0.15s;
	}

	.source-item:hover {
		background: var(--hover-bg);
	}

	.source-item input[type='checkbox'] {
		cursor: pointer;
		margin: 0;
	}

	.source-name {
		font-size: 13px;
		color: var(--text);
	}
</style>

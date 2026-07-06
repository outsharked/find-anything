<script lang="ts">
	let {
		items = [],
		activeIndex = -1,
		loading = false,
		sourcePhase = false,
		onSelect,
		onHover
	}: {
		items?: string[];
		activeIndex?: number;
		loading?: boolean;
		/** True when showing source names, false when showing directory names. */
		sourcePhase?: boolean;
		onSelect?: (name: string) => void;
		onHover?: (index: number) => void;
	} = $props();

	let listEl: HTMLDivElement;

	// Scroll the active item into view when it changes via keyboard.
	$effect(() => {
		if (listEl && activeIndex >= 0) {
			const el = listEl.children[activeIndex] as HTMLElement | undefined;
			el?.scrollIntoView({ block: 'nearest' });
		}
	});
</script>

<div class="typeahead" bind:this={listEl} role="listbox">
	{#if loading}
		<div class="typeahead-loading">Loading…</div>
	{:else if items.length === 0}
		<div class="typeahead-empty">No matches</div>
	{:else}
		{#each items as item, i}
			<button
				class="typeahead-item"
				class:active={i === activeIndex}
				role="option"
				aria-selected={i === activeIndex}
				onmousedown={(e) => { e.preventDefault(); onSelect?.(item); }}
				onmouseenter={() => onHover?.(i)}
			>
				<span class="typeahead-icon">{sourcePhase ? '◉' : '▸'}</span>
				<span class="typeahead-name">{item}</span>
				{#if !sourcePhase}<span class="typeahead-slash">/</span>{/if}
			</button>
		{/each}
	{/if}
</div>

<style>
	.typeahead {
		position: absolute;
		top: calc(100% + 4px);
		left: 0;
		right: 0;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
		z-index: 300;
		max-height: 260px;
		overflow-y: auto;
		font-size: 13px;
	}

	.typeahead-item {
		display: flex;
		align-items: center;
		gap: 6px;
		width: 100%;
		padding: 7px 12px;
		background: none;
		border: none;
		color: var(--text);
		cursor: pointer;
		text-align: left;
		font: inherit;
		font-size: 13px;
	}

	.typeahead-item:hover,
	.typeahead-item.active {
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
	}

	.typeahead-item.active {
		color: var(--accent);
	}

	.typeahead-icon {
		font-size: 10px;
		color: var(--text-dim);
		flex-shrink: 0;
	}

	.typeahead-name {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-mono);
	}

	.typeahead-slash {
		color: var(--text-dim);
		font-family: var(--font-mono);
		flex-shrink: 0;
	}

	.typeahead-loading,
	.typeahead-empty {
		padding: 8px 12px;
		color: var(--text-dim);
		font-size: 12px;
	}

	@media (max-width: 768px) {
		.typeahead { display: none; }
	}
</style>

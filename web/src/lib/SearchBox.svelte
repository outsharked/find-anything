<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let query = '';
	export let mode = 'fuzzy';
	export let searching = false;
	export let isTyping = false;
	export let nlpHighlightSpan: [number, number] | undefined = undefined;

	const dispatch = createEventDispatcher<{
		change: { query: string; mode: string };
	}>();

	let debounceTimer: ReturnType<typeof setTimeout>;

	function handleInput() {
		isTyping = true;
		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(() => {
			isTyping = false;
			dispatch('change', { query, mode });
		}, 500);
	}

	function handleModeChange() {
		isTyping = false;
		dispatch('change', { query, mode });
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			clearTimeout(debounceTimer);
			isTyping = false;
			dispatch('change', { query, mode });
		}
	}

	export function focus() {
		inputEl?.focus();
	}

	let inputEl: HTMLInputElement;
	let backdropEl: HTMLDivElement;

	$: showSpinner = isTyping || searching;

	$: hlBefore = nlpHighlightSpan ? query.slice(0, nlpHighlightSpan[0]) : '';
	$: hlMiddle = nlpHighlightSpan ? query.slice(nlpHighlightSpan[0], nlpHighlightSpan[1]) : '';
	$: hlAfter  = nlpHighlightSpan ? query.slice(nlpHighlightSpan[1]) : '';

	function syncScroll() {
		if (backdropEl) backdropEl.scrollLeft = inputEl.scrollLeft;
	}

	// Sync backdrop scroll whenever the highlight span changes (e.g. after debounce).
	$: if (nlpHighlightSpan && backdropEl && inputEl) {
		backdropEl.scrollLeft = inputEl.scrollLeft;
	}
</script>

<div class="search-box">
	<select bind:value={mode} on:change={handleModeChange} class="mode-select">
		<option value="fuzzy">Fuzzy</option>
		<option value="document">Document</option>
		<option value="exact">Exact</option>
		<option value="regex">Regex</option>
	</select>
	<div class="input-wrap">
		{#if nlpHighlightSpan}
			<div class="backdrop" bind:this={backdropEl} aria-hidden="true">{hlBefore}<span class="date-hl">{hlMiddle}</span>{hlAfter}</div>
		{/if}
		<input
			bind:this={inputEl}
			bind:value={query}
			on:input={handleInput}
			on:keydown={handleKeydown}
			on:scroll={syncScroll}
			type="text"
			placeholder="Search…"
			autocomplete="off"
			spellcheck="false"
			class="search-input"
			class:has-highlight={!!nlpHighlightSpan}
		/>
	</div>
	{#if showSpinner}
		<div class="spinner" title="Searching...">
			<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
				<circle cx="12" cy="12" r="10" stroke="currentColor" stroke-width="3" opacity="0.25"/>
				<path d="M12 2a10 10 0 0 1 10 10" stroke="currentColor" stroke-width="3" stroke-linecap="round"/>
			</svg>
		</div>
	{/if}
</div>

<style>
	.search-box {
		display: flex;
		align-items: center;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		overflow: hidden;
	}

	.input-wrap {
		flex: 1;
		position: relative;
		overflow: hidden;
	}

	.search-input {
		width: 100%;
		padding: 8px 12px;
		background: transparent;
		border: none;
		color: var(--text);
		outline: none;
		font: inherit;
		box-sizing: border-box;
	}

	.search-input.has-highlight {
		color: transparent;
		caret-color: var(--text);
	}

	.search-input::placeholder {
		color: var(--text-dim);
	}

	.backdrop {
		position: absolute;
		inset: 0;
		padding: 8px 12px;
		font: inherit;
		white-space: pre;
		overflow: scroll;
		scrollbar-width: none;
		color: var(--text);
		pointer-events: none;
		box-sizing: border-box;
		line-height: normal;
	}

	.backdrop::-webkit-scrollbar {
		display: none;
	}

	.date-hl {
		background: color-mix(in srgb, #3fb950 30%, transparent);
		border-radius: 2px;
	}

	.mode-select {
		padding: 8px 10px;
		background: var(--bg-hover);
		border: none;
		border-right: 1px solid var(--border);
		color: var(--text-muted);
		cursor: pointer;
		outline: none;
	}

	.mode-select:hover {
		color: var(--text);
	}

	.spinner {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		margin-right: 4px;
		flex-shrink: 0;
	}

	.spinner svg {
		width: 16px;
		height: 16px;
		color: var(--accent);
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}
</style>

<script lang="ts">
	import IconSpinner from '$lib/icons/IconSpinner.svelte';
	import IconClear from '$lib/icons/IconClear.svelte';

	let {
		query: queryProp = '',
		searching = false,
		isTyping = $bindable(false),
		nlpHighlightSpan = undefined,
		onChange,
		onRawInput,
		onFocus,
		onBlur
	}: {
		query?: string;
		searching?: boolean;
		isTyping?: boolean;
		nlpHighlightSpan?: [number, number] | undefined;
		onChange?: (detail: { query: string }) => void;
		/** Fires immediately on every keystroke — used by typeahead, no debounce. */
		onRawInput?: (detail: { query: string }) => void;
		onFocus?: () => void;
		onBlur?: () => void;
	} = $props();

	// `query` is seeded from the prop but then diverges locally as the user
	// types (the parent isn't bound to it — it only finds out via onChange/
	// onRawInput). Re-sync from the prop only when the parent's own value
	// actually changes (e.g. browser back/forward), not on every render.
	// svelte-ignore state_referenced_locally
	let query = $state(queryProp);
	$effect(() => {
		query = queryProp;
	});

	let debounceTimer: ReturnType<typeof setTimeout>;
	let prevLength = 0;

	function handleInput() {
		const isDeletion = query.length < prevLength;
		prevLength = query.length;
		onRawInput?.({ query });

		if (isDeletion) {
			// Deleting characters: cancel any pending search and don't start a new one.
			// The user can press Enter or type a new character to trigger a search.
			clearTimeout(debounceTimer);
			isTyping = false;
			return;
		}

		isTyping = true;
		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(() => {
			isTyping = false;
			onChange?.({ query });
		}, 500);
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			clearTimeout(debounceTimer);
			isTyping = false;
			onChange?.({ query });
		}
	}

	function clearQuery() {
		query = '';
		isTyping = false;
		clearTimeout(debounceTimer);
		onChange?.({ query: '' });
		inputEl?.focus();
	}

	export function focus() {
		inputEl?.focus();
	}

	let inputEl: HTMLInputElement | undefined = $state();
	let backdropEl: HTMLDivElement | undefined = $state();

	let showSpinner = $derived(isTyping || searching);

	let hlBefore = $derived(nlpHighlightSpan ? query.slice(0, nlpHighlightSpan[0]) : '');
	let hlMiddle = $derived(nlpHighlightSpan ? query.slice(nlpHighlightSpan[0], nlpHighlightSpan[1]) : '');
	let hlAfter  = $derived(nlpHighlightSpan ? query.slice(nlpHighlightSpan[1]) : '');

	function syncScroll() {
		if (backdropEl && inputEl) backdropEl.scrollLeft = inputEl.scrollLeft;
	}

	// Sync backdrop scroll whenever the highlight span changes (e.g. after debounce).
	$effect(() => {
		if (nlpHighlightSpan && backdropEl && inputEl) {
			backdropEl.scrollLeft = inputEl.scrollLeft;
		}
	});
</script>

<div class="search-box">
	<div class="input-wrap">
		{#if nlpHighlightSpan}
			<div class="backdrop" bind:this={backdropEl} aria-hidden="true">{hlBefore}<span class="date-hl">{hlMiddle}</span>{hlAfter}</div>
		{/if}
		<input
			bind:this={inputEl}
			bind:value={query}
			oninput={handleInput}
			onkeydown={handleKeydown}
			onscroll={syncScroll}
			onfocus={() => onFocus?.()}
			onblur={() => onBlur?.()}
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
			<IconSpinner />
		</div>
	{:else if query}
		<button class="clear-btn" onclick={clearQuery} aria-label="Clear search">
			<IconClear />
		</button>
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

	.spinner {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		margin-right: 4px;
		flex-shrink: 0;
	}

	.spinner :global(svg) {
		width: 16px;
		height: 16px;
		color: var(--accent);
		animation: spin 0.8s linear infinite;
	}

	.clear-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 32px;
		height: 32px;
		margin-right: 4px;
		flex-shrink: 0;
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		color: var(--text-muted);
	}

	.clear-btn:hover {
		color: var(--text);
	}

	.clear-btn :global(svg) {
		width: 16px;
		height: 16px;
	}

	@keyframes spin {
		from { transform: rotate(0deg); }
		to { transform: rotate(360deg); }
	}
</style>

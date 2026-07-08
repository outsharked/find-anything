<script lang="ts">
	import { tick } from 'svelte';
	import { parseGoToLineInput } from '$lib/goToLine';

	let {
		open = false,
		onSubmit,
		onClose
	}: {
		/** Set to true to show the dialog. */
		open?: boolean;
		onSubmit?: (line: number) => void;
		onClose?: () => void;
	} = $props();

	let value = $state('');
	let inputEl: HTMLInputElement | undefined = $state();
	let invalid = $state(false);

	// $effect only tracks reads made synchronously during the callback, so
	// reading inputEl inside the tick().then() callback below does not
	// re-trigger this effect — no previous-value guard needed (same pattern
	// as CommandPalette's open-reset effect).
	$effect(() => {
		if (open) {
			value = '';
			invalid = false;
			tick().then(() => inputEl?.focus());
		}
	});

	function close() {
		onClose?.();
	}

	function submit() {
		const line = parseGoToLineInput(value);
		if (line === null) {
			invalid = true;
			return;
		}
		onSubmit?.(line);
		close();
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			close();
		} else if (e.key === 'Enter') {
			submit();
		}
	}
</script>

{#if open}
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="gtl-backdrop" onclick={close} onkeydown={onKeydown}>
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="gtl-panel" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
			<div class="gtl-input-wrap">
				<span class="gtl-icon">:</span>
				<input
					bind:this={inputEl}
					bind:value
					class="gtl-input"
					class:invalid
					type="text"
					inputmode="numeric"
					placeholder="Go to line…"
					autocomplete="off"
					spellcheck="false"
					oninput={() => (invalid = false)}
					onkeydown={onKeydown}
				/>
			</div>
			{#if invalid}
				<div class="gtl-status">Enter a line number, e.g. 5000</div>
			{/if}
		</div>
	</div>
{/if}

<style>
	.gtl-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.5);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding-top: 15vh;
		z-index: 1000;
	}

	.gtl-panel {
		width: min(320px, 90vw);
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		border-radius: 8px;
		overflow: hidden;
		box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
	}

	.gtl-input-wrap {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 10px 14px;
	}

	.gtl-icon {
		color: var(--text-muted);
		font-size: 16px;
		flex-shrink: 0;
	}

	.gtl-input {
		flex: 1;
		background: none;
		border: none;
		outline: none;
		color: var(--text);
		font-size: 14px;
		font-family: var(--font-mono);
	}

	.gtl-input.invalid {
		color: #f85149;
	}

	.gtl-status {
		padding: 0 14px 10px;
		color: #f85149;
		font-size: 12px;
	}
</style>

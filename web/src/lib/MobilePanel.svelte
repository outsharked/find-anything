<script lang="ts">
	import type { Snippet } from 'svelte';
	import IconBack from '$lib/icons/IconBack.svelte';

	let {
		open = $bindable(false),
		title,
		children
	}: { open?: boolean; title: string; children?: Snippet } = $props();

	// Portal: physically moves the panel DOM node to document.body so it is
	// never clipped or hidden by any ancestor's overflow/display/transform CSS.
	function portal(node: HTMLElement) {
		document.body.appendChild(node);
		return { destroy() { node.remove(); } };
	}
</script>

{#if open}
	<div class="mobile-panel" use:portal>
		<div class="mobile-panel-header">
			<button class="back-btn" onclick={() => (open = false)} aria-label="Close">
				<IconBack />
			</button>
			<span class="mobile-panel-title">{title}</span>
		</div>
		<div class="mobile-panel-body">
			{@render children?.()}
		</div>
	</div>
{/if}

<style>
	/* Desktop: never shown — the caller handles desktop UI itself */
	.mobile-panel { display: none; }

	@media (max-width: 768px) {
		.mobile-panel {
			position: fixed;
			top: 0; right: 0; bottom: 0; left: 0;
			z-index: 2000;
			background: var(--bg-secondary);
			display: flex;
			flex-direction: column;
		}

		.mobile-panel-header {
			display: flex;
			align-items: center;
			gap: 12px;
			padding: 12px 16px;
			border-bottom: 1px solid var(--border);
			flex-shrink: 0;
		}

		.back-btn {
			background: none;
			border: none;
			padding: 4px;
			cursor: pointer;
			color: var(--text);
			display: flex;
			align-items: center;
			justify-content: center;
			border-radius: 4px;
			min-width: 32px;
			min-height: 32px;
		}

		.back-btn:hover { background: var(--bg-hover); }

		.mobile-panel-title {
			font-size: 16px;
			font-weight: 600;
			color: var(--text);
		}

		.mobile-panel-body {
			flex: 1;
			overflow-y: auto;
		}
	}
</style>

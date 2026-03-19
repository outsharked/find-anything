<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import {
		type LineSelection,
		selectionSet,
		firstLine,
		toggleLine
	} from '$lib/lineSelection';

	/** Syntax-highlighted HTML lines from highlightFile(). */
	export let codeLines: string[];
	/** Maps render index (0-based) → original line_number. */
	export let lineOffsets: number[];
	/** Currently selected lines. */
	export let selection: LineSelection = [];
	/** Whether to enable soft word-wrap. */
	export let wordWrap: boolean = false;
	/** Number of spaces a tab character occupies. */
	export let tabWidth: number = 4;

	const dispatch = createEventDispatcher<{
		lineselect: { selection: LineSelection };
	}>();

	$: highlightedSet = selectionSet(selection);
	$: arrowLine = firstLine(selection);

	function handleLineClick(lineNum: number, e: MouseEvent) {
		let next: LineSelection;
		if (e.ctrlKey || e.metaKey) {
			next = toggleLine(selection, lineNum);
		} else if (e.shiftKey && selection.length > 0) {
			const anchor = firstLine(selection)!;
			next = [anchor <= lineNum ? [anchor, lineNum] : [lineNum, anchor]];
		} else {
			next = [lineNum];
		}
		selection = next;
		dispatch('lineselect', { selection: next });
	}
</script>

<table class="code-table" cellspacing="0" cellpadding="0" style="tab-size: {tabWidth}">
	<tbody>
		{#each codeLines as line, i}
			{@const lineNum = lineOffsets[i] ?? i + 1}
			<!-- svelte-ignore a11y-click-events-have-key-events -->
			<!-- svelte-ignore a11y-no-static-element-interactions -->
			<tr
				id="line-{lineNum}"
				class="code-row"
				class:target={highlightedSet.has(lineNum)}
				on:click={(e) => handleLineClick(lineNum, e)}
			>
				<td class="td-ln">{lineNum}</td>
				<td class="td-arrow">{lineNum === arrowLine ? '▶' : ''}</td>
				<td class="td-code" class:wrap={wordWrap}><code>{@html line}</code></td>
			</tr>
		{/each}
	</tbody>
</table>

<style>
	.code-table {
		width: 100%;
		border-collapse: collapse;
		font-family: var(--font-mono);
		font-size: 13px;
		line-height: 1.6;
	}

	.code-row {
		border-left: 2px solid transparent;
		cursor: pointer;
	}

	.code-row:hover {
		background: var(--bg-hover, rgba(255, 255, 255, 0.04));
	}

	.code-row.target {
		background: var(--match-line-bg);
		border-left-color: var(--match-border);
	}

	.td-ln {
		width: 1%;
		min-width: 52px;
		white-space: nowrap;
		padding: 0 12px 0 8px;
		text-align: right;
		color: var(--text-dim);
		user-select: none;
		vertical-align: top;
	}

	.td-arrow {
		width: 16px;
		white-space: nowrap;
		color: var(--accent);
		font-size: 10px;
		user-select: none;
		vertical-align: top;
	}

	.td-code {
		width: 100%;
		padding: 0 16px 0 4px;
		white-space: pre;
		vertical-align: top;
	}

	.td-code.wrap {
		white-space: pre-wrap;
		word-break: break-word;
	}
</style>

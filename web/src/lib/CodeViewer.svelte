<script lang="ts">
	import {
		type LineSelection,
		selectionSet,
		firstLine,
		toggleLine
	} from '$lib/lineSelection';

	let {
		codeLines,
		lineOffsets,
		selection = [],
		wordWrap = false,
		tabWidth = 4,
		onLineSelect
	}: {
		/** Syntax-highlighted HTML lines from highlightFile(). */
		codeLines: string[];
		/** Maps render index (0-based) → original line_number. */
		lineOffsets: number[];
		/** Currently selected lines — controlled by the caller, not mutated locally. */
		selection?: LineSelection;
		/** Whether to enable soft word-wrap. */
		wordWrap?: boolean;
		/** Number of spaces a tab character occupies. */
		tabWidth?: number;
		onLineSelect?: (selection: LineSelection) => void;
	} = $props();

	let highlightedSet = $derived(selectionSet(selection));
	let arrowLine = $derived(firstLine(selection));

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
		onLineSelect?.(next);
	}
</script>

<table class="code-table" cellspacing="0" cellpadding="0" style="tab-size: {tabWidth}">
	<tbody>
		{#each codeLines as line, i}
			{@const lineNum = lineOffsets[i] ?? i + 1}
			<!-- svelte-ignore a11y_click_events_have_key_events -->
			<!-- svelte-ignore a11y_no_static_element_interactions -->
			<tr
				id="line-{lineNum}"
				class="code-row"
				class:target={highlightedSet.has(lineNum)}
				onclick={(e) => handleLineClick(lineNum, e)}
			>
				<td class="td-ln">{lineNum}</td>
				<td class="td-arrow" style:visibility={lineNum === arrowLine ? 'visible' : 'hidden'}>▶</td>
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

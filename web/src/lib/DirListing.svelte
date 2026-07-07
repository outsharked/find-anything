<script lang="ts">
	import { listDir } from '$lib/api';
	import type { DirEntry } from '$lib/api';
	import { liveEvent } from '$lib/liveUpdates';

	let {
		source,
		prefix,
		onOpenFile,
		onOpenDir
	}: {
		source: string;
		prefix: string; // "" for root, "foo/bar/" for subdirectory
		onOpenFile?: (detail: { source: string; path: string; kind: string }) => void;
		onOpenDir?: (detail: { prefix: string }) => void;
	} = $props();

	// Compute the parent prefix for the "up" button.
	// Handles both "/" separators (directories) and "::" separators (archive roots).
	function parentPrefix(p: string): string | null {
		if (!p) return null;
		// Strip the trailing separator ("/" or "::")
		const s = p.endsWith('::') ? p.slice(0, -2) : p.slice(0, -1);
		const lastSlash = s.lastIndexOf('/');
		const lastSep = s.lastIndexOf('::');
		if (lastSlash === -1 && lastSep === -1) return ''; // one level above root
		if (lastSep > lastSlash) return s.slice(0, lastSep + 2); // keep "::"
		return s.slice(0, lastSlash + 1); // keep "/"
	}

	let parent = $derived(parentPrefix(prefix));

	let entries: DirEntry[] = $state([]);
	let loading = $state(true);
	let error: string | null = $state(null);

	// Reload when prefix changes.
	$effect(() => {
		load(source, prefix);
	});

	// Reload when a live event affects the current directory.
	$effect(() => {
		if ($liveEvent && $liveEvent.source === source) {
			const ev = $liveEvent;
			const parentDir = dirOf(ev.path);
			const newParentDir = ev.new_path ? dirOf(ev.new_path) : null;
			if (parentDir === prefix || newParentDir === prefix) {
				load(source, prefix);
			}
		}
	});

	function dirOf(p: string): string {
		const i = p.lastIndexOf('/');
		return i >= 0 ? p.slice(0, i + 1) : '';
	}

	async function load(_source: string, _prefix: string) {
		loading = true;
		error = null;
		try {
			const resp = await listDir(_source, _prefix);
			entries = resp.entries;
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	}

	function formatSize(bytes: number | undefined): string {
		if (bytes == null) return '';
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	}

	function formatDate(mtime: number | undefined): string {
		if (mtime == null) return '';
		return new Date(mtime * 1000).toLocaleDateString();
	}
</script>

<div class="listing">
	{#if loading}
		<div class="status">Loading…</div>
	{:else if error}
		<div class="status error">{error}</div>
	{:else if entries.length === 0}
		<div class="status">Empty directory.</div>
	{:else}
		<table class="table">
			<thead>
				<tr>
					<th class="col-name">Name</th>
					<th class="col-kind">Kind</th>
					<th class="col-size">Size</th>
					<th class="col-date">Modified</th>
				</tr>
			</thead>
			<tbody>
				{#if parent !== null}
					<tr class="row row--up" onclick={() => onOpenDir?.({ prefix: parent ?? '' })}>
						<td class="col-name" colspan="4">
							<span class="icon-up">↑</span>
							<span class="name">..</span>
						</td>
					</tr>
				{/if}
				{#each entries as entry (entry.path)}
					<tr
						class="row"
						class:row--dir={entry.entry_type === 'dir'}
						onclick={() =>
							entry.entry_type === 'dir'
								? onOpenDir?.({ prefix: entry.path })
								: onOpenFile?.({ source, path: entry.path, kind: entry.kind ?? 'text' })}
					>
						<td class="col-name">
							<span class="icon">{entry.entry_type === 'dir' ? '▸' : '·'}</span>
							<span class="name">{entry.name}{entry.entry_type === 'dir' ? '/' : ''}</span>
						</td>
						<td class="col-kind">{entry.kind ?? ''}</td>
						<td class="col-size">{formatSize(entry.size)}</td>
						<td class="col-date">{formatDate(entry.mtime)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	{/if}
</div>

<style>
	.listing {
		height: 100%;
		overflow-y: auto;
	}

	.status {
		padding: 24px;
		color: var(--text-muted);
		text-align: center;
	}

	.status.error {
		color: #f85149;
	}

	.table {
		width: 100%;
		border-collapse: collapse;
		font-size: 13px;
	}

	thead th {
		padding: 6px 12px;
		text-align: left;
		color: var(--text-muted);
		font-weight: 500;
		border-bottom: 1px solid var(--border);
		background: var(--bg-secondary);
		position: sticky;
		top: 0;
		white-space: nowrap;
	}

	.row {
		cursor: pointer;
	}

	.row:hover td {
		background: var(--bg-hover, rgba(255, 255, 255, 0.04));
	}

	.row td {
		padding: 5px 12px;
		border-bottom: 1px solid var(--border);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.col-name {
		width: 100%;
	}

	.col-kind,
	.col-size,
	.col-date {
		color: var(--text-muted);
		text-align: right;
		min-width: 60px;
	}

	.icon {
		display: inline-block;
		width: 14px;
		text-align: center;
		margin-right: 4px;
		color: var(--text-muted);
		font-size: 11px;
	}

	.row--dir .name {
		color: var(--accent, #58a6ff);
	}

	.row--up .name {
		color: var(--text-muted);
	}

	.icon-up {
		display: inline-block;
		width: 14px;
		text-align: center;
		margin-right: 0;
		color: var(--text-muted);
		font-size: 14px;
	}

	.row--dir .icon {
		font-size: 18px;
		width: 14px;
		margin-right: 0;
		color: var(--accent, #58a6ff);
	}
</style>

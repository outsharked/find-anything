<script lang="ts">
	import AppLogo from './AppLogo.svelte';

	let {
		filename,
		mtime,
		linkCode,
		source,
		path,
		archivePath
	}: {
		filename: string;
		mtime: number;
		linkCode: string;
		source: string;
		path: string;
		archivePath: string | null;
	} = $props();

	let dateStr = $derived(new Date(mtime * 1000).toLocaleDateString(undefined, {
		year: 'numeric',
		month: '2-digit',
		day: '2-digit'
	}));

	let compositePath = $derived(archivePath ? `${path}::${archivePath}` : path);

	let rawUrl = $derived.by(() => {
		const u = new URL('/api/v1/raw', location.origin);
		u.searchParams.set('link_code', linkCode);
		u.searchParams.set('source', source);
		u.searchParams.set('path', compositePath);
		return u.toString();
	});

	let downloadUrl = $derived(rawUrl + '&download=1');

	let openInAppUrl = $derived.by(() => {
		const u = new URL('/', location.origin);
		u.searchParams.set('view', 'file');
		u.searchParams.set('fsource', source);
		u.searchParams.set('path', path);
		if (archivePath) u.searchParams.set('apath', archivePath);
		return u.toString();
	});
</script>

<header class="direct-header">
	<a class="brand" href="/"><AppLogo /></a>
	<span class="filename">{filename}</span>
	<span class="date">{dateStr}</span>
	<div class="actions">
		<a class="btn" href={downloadUrl} download={filename}>⬇ Download</a>
		<a class="btn btn-secondary" href={openInAppUrl}>Open in app</a>
	</div>
</header>

<style>
	.direct-header {
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 8px 20px;
		background: var(--bg-secondary, #1a1a2e);
		border-bottom: 1px solid var(--border, #333);
		flex-shrink: 0;
		min-height: 44px;
		flex-wrap: wrap;
	}

	.brand {
		color: var(--accent);
		text-decoration: none;
		flex-shrink: 0;
	}

	.brand:hover {
		text-decoration: underline;
	}

	.filename {
		font-family: var(--font-mono, monospace);
		font-size: 13px;
		color: var(--text, #cdd6f4);
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.date {
		font-size: 12px;
		color: var(--text-muted, #888);
		flex-shrink: 0;
	}

	.actions {
		display: flex;
		gap: 8px;
		flex-shrink: 0;
	}

	.btn {
		font-size: 12px;
		padding: 4px 12px;
		border-radius: var(--radius, 4px);
		border: 1px solid var(--border, #333);
		background: var(--bg-tertiary, #222);
		color: var(--text, #cdd6f4);
		text-decoration: none;
		cursor: pointer;
		white-space: nowrap;
	}

	.btn:hover {
		border-color: var(--accent, #7aa2f7);
		color: var(--accent, #7aa2f7);
	}

	.btn-secondary {
		opacity: 0.7;
	}
</style>

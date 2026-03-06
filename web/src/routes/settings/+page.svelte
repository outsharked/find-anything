<script lang="ts">
	import { goto, replaceState } from '$app/navigation';
	import { page } from '$app/stores';
	import Preferences from '$lib/Preferences.svelte';
	import StatsPanel from '$lib/StatsPanel.svelte';
	import ErrorsPanel from '$lib/ErrorsPanel.svelte';
	import About from '$lib/About.svelte';
	// Declare params prop to silence runtime "unknown prop" warning.
	export let params: Record<string, string>;
	const _params = params;

	let activeSection = $page.url.searchParams.get('section') ?? 'preferences';

	function setSection(section: string) {
		activeSection = section;
		replaceState(`?section=${section}`, {});
	}

	function goBack() {
		if (history.length > 1) {
			history.back();
		} else {
			goto('/');
		}
	}

	function handleErrorNavigate(e: CustomEvent<{ source: string; path: string }>) {
		const p = new URLSearchParams();
		p.set('view', 'file');
		p.set('fsource', e.detail.source);
		p.set('path', e.detail.path);
		goto(`/?${p.toString()}`);
	}
</script>

<div class="settings-page">
	<!-- Header -->
	<div class="header">
		<button class="back-btn" on:click={goBack} title="Go back">←</button>
		<a class="logo" href="/">find-anything</a>
	</div>

	<!-- Body: left nav + content -->
	<div class="body">
		<nav class="sidebar">
			<button
				class="nav-item"
				class:active={activeSection === 'preferences'}
				on:click={() => setSection('preferences')}
			>
				Preferences
			</button>
			<button
				class="nav-item"
				class:active={activeSection === 'stats'}
				on:click={() => setSection('stats')}
			>
				Stats
			</button>
			<button
				class="nav-item"
				class:active={activeSection === 'errors'}
				on:click={() => setSection('errors')}
			>
				Errors
			</button>
			<button
				class="nav-item"
				class:active={activeSection === 'about'}
				on:click={() => setSection('about')}
			>
				About
			</button>
		</nav>

		<main class="content">
			{#if activeSection === 'preferences'}
				<h2 class="content-title">Preferences</h2>
				<Preferences />
			{:else if activeSection === 'stats'}
				<h2 class="content-title">Index Statistics</h2>
				<StatsPanel />
			{:else if activeSection === 'errors'}
				<h2 class="content-title">Indexing Errors</h2>
				<ErrorsPanel on:navigate={handleErrorNavigate} />
			{:else if activeSection === 'about'}
				<h2 class="content-title">About</h2>
				<About />
			{:else}
				<h2 class="content-title">Preferences</h2>
				<Preferences />
			{/if}
		</main>
	</div>
</div>

<style>
	.settings-page {
		display: flex;
		flex-direction: column;
		height: 100vh;
		background: var(--bg);
		color: var(--text);
	}

	/* Header */
	.header {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 10px 20px;
		background: var(--bg-secondary);
		border-bottom: 1px solid var(--border);
		flex-shrink: 0;
	}

	.back-btn {
		background: none;
		border: none;
		color: var(--text-muted);
		font-size: 18px;
		padding: 2px 6px;
		border-radius: 4px;
		cursor: pointer;
		line-height: 1;
	}

	.back-btn:hover {
		color: var(--text);
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
	}

	.logo {
		font-size: 14px;
		font-weight: 600;
		color: var(--text);
		text-decoration: none;
	}

	.logo:hover {
		color: var(--accent, #58a6ff);
	}

	/* Body */
	.body {
		display: flex;
		flex: 1;
		min-height: 0;
	}

	/* Left nav */
	.sidebar {
		width: 180px;
		flex-shrink: 0;
		border-right: 1px solid var(--border);
		padding: 16px 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.nav-item {
		display: block;
		width: 100%;
		text-align: left;
		background: none;
		border: none;
		color: var(--text-muted);
		font-size: 13px;
		padding: 7px 20px;
		cursor: pointer;
		border-radius: 0;
	}

	.nav-item:hover {
		color: var(--text);
		background: var(--bg-hover, rgba(255, 255, 255, 0.05));
	}

	.nav-item.active {
		color: var(--text);
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
		font-weight: 500;
		border-left: 2px solid var(--accent, #58a6ff);
		padding-left: 18px;
	}

	/* Content area */
	.content {
		flex: 1;
		padding: 24px 32px;
		overflow-y: auto;
		min-width: 0;
	}

	.content-title {
		font-size: 16px;
		font-weight: 600;
		color: var(--text);
		margin: 0 0 20px;
	}
</style>

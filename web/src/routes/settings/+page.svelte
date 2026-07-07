<script lang="ts">
	import { onMount } from 'svelte';
	import { goto, replaceState } from '$app/navigation';
	import { page } from '$app/stores';
	import Preferences from '$lib/Preferences.svelte';
	import StatsPanel from '$lib/StatsPanel.svelte';
	import ErrorsPanel from '$lib/ErrorsPanel.svelte';
	import AdminPanel from '$lib/AdminPanel.svelte';
	import About from '$lib/About.svelte';

	let activeSection = $state($page.url.searchParams.get('section') ?? 'preferences');
	let isMobile = $state(false);

	function setSection(section: string) {
		activeSection = section;
		replaceState(`?section=${section}`, {});
	}

	function toggleSection(section: string) {
		setSection(activeSection === section ? '' : section);
	}

	function goBack() {
		if (history.length > 1) {
			history.back();
		} else {
			goto('/');
		}
	}

	function handleErrorNavigate(source: string, path: string) {
		const p = new URLSearchParams();
		p.set('view', 'file');
		p.set('fsource', source);
		p.set('path', path);
		goto(`/?${p.toString()}`);
	}

	onMount(() => {
		const mq = window.matchMedia('(max-width: 768px)');
		isMobile = mq.matches;
		const handler = (e: MediaQueryListEvent) => { isMobile = e.matches; };
		mq.addEventListener('change', handler);
		return () => mq.removeEventListener('change', handler);
	});

	const sections = [
		{ id: 'preferences', label: 'Preferences' },
		{ id: 'stats',       label: 'Stats' },
		{ id: 'errors',      label: 'Errors' },
		{ id: 'admin',       label: 'Admin' },
		{ id: 'about',       label: 'About' },
	] as const;
</script>

<div class="settings-page">
	<!-- Header -->
	<div class="header">
		<button class="back-btn" onclick={goBack} title="Go back">←</button>
		<a class="logo" href="/">find-anything</a>
	</div>

	<!-- Body -->
	<div class="body">
		{#if !isMobile}
			<!-- Desktop: left nav + content pane -->
			<nav class="sidebar">
				{#each sections as s}
					<button
						class="nav-item"
						class:active={activeSection === s.id}
						onclick={() => setSection(s.id)}
					>{s.label}</button>
				{/each}
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
					<ErrorsPanel onNavigate={handleErrorNavigate} />
				{:else if activeSection === 'admin'}
					<h2 class="content-title">Admin</h2>
					<AdminPanel />
				{:else if activeSection === 'about'}
					<h2 class="content-title">About</h2>
					<About />
				{:else}
					<h2 class="content-title">Preferences</h2>
					<Preferences />
				{/if}
			</main>
		{:else}
			<!-- Mobile: accordion -->
			<div class="accordion">
				{#each sections as s}
					<div class="accordion-item">
						<button
							class="accordion-header"
							class:open={activeSection === s.id}
							onclick={() => toggleSection(s.id)}
						>
							<span>{s.label}</span>
							<svg
								class="acc-chevron"
								class:open={activeSection === s.id}
								width="14" height="14" viewBox="0 0 14 14"
								fill="none" stroke="currentColor" stroke-width="1.8"
								stroke-linecap="round" stroke-linejoin="round"
								aria-hidden="true"
							>
								<polyline points="2,4 7,10 12,4"/>
							</svg>
						</button>
						{#if activeSection === s.id}
							<div class="accordion-body">
								{#if s.id === 'preferences'}<Preferences />
								{:else if s.id === 'stats'}<StatsPanel />
								{:else if s.id === 'errors'}<ErrorsPanel onNavigate={handleErrorNavigate} />
								{:else if s.id === 'admin'}<AdminPanel />
								{:else if s.id === 'about'}<About />
								{/if}
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
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

	.logo:hover { color: var(--accent, #58a6ff); }

	/* Body */
	.body {
		display: flex;
		flex: 1;
		min-height: 0;
	}

	/* ── Desktop: left nav ───────────────────────────────── */

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

	/* ── Mobile: accordion ───────────────────────────────── */

	.accordion {
		flex: 1;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
	}

	.accordion-item {
		border-bottom: 1px solid var(--border);
	}

	.accordion-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		width: 100%;
		padding: 16px 20px;
		background: none;
		border: none;
		color: var(--text);
		font-size: 15px;
		font-weight: 500;
		cursor: pointer;
		text-align: left;
		min-height: 52px;
	}

	.accordion-header:hover {
		background: var(--bg-hover);
	}

	.accordion-header.open {
		color: var(--accent);
		background: var(--match-bg);
	}

	.acc-chevron {
		flex-shrink: 0;
		transition: transform 0.2s ease;
		color: var(--text-dim);
	}

	.acc-chevron.open {
		transform: rotate(180deg);
		color: var(--accent);
	}

	.accordion-body {
		padding: 20px;
		border-top: 1px solid var(--border);
		background: var(--bg);
		overflow-x: hidden;
	}
</style>

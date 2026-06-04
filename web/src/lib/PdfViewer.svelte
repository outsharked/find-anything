<script lang="ts">
	import { onMount } from 'svelte';

	export let src: string;

	// Android Chrome does not render PDFs inline in iframes — use PDF.js there.
	const isMobile = typeof navigator !== 'undefined' && /Android/i.test(navigator.userAgent);

	let loaded = false;
	let pdfError = false;
	let canvasContainer: HTMLDivElement;
	let renderedSrc = '';

	// Reset spinner when src changes (iframe path only — mobile reset happens in renderPdf).
	$: if (!isMobile) { src; loaded = false; }

	// Re-render when src changes after the component is mounted (canvasContainer guard
	// prevents this running before bind:this is resolved on the initial mount pass).
	$: if (isMobile && src !== renderedSrc && canvasContainer) {
		renderPdf(src);
	}

	onMount(() => {
		if (isMobile) renderPdf(src);
	});

	async function renderPdf(forSrc: string) {
		renderedSrc = forSrc;
		loaded = false;
		pdfError = false;

		try {
			// Dynamic import — only fetched the first time a PDF is viewed on mobile.
			const pdfjsLib = await import('pdfjs-dist');
			pdfjsLib.GlobalWorkerOptions.workerSrc = new URL(
				'pdfjs-dist/build/pdf.worker.mjs',
				import.meta.url
			).href;

			const pdf = await pdfjsLib.getDocument({ url: forSrc }).promise;
			if (!canvasContainer) return;
			canvasContainer.innerHTML = '';

			for (let pageNum = 1; pageNum <= pdf.numPages; pageNum++) {
				const page = await pdf.getPage(pageNum);
				const baseViewport = page.getViewport({ scale: 1 });
				const scale = (canvasContainer.clientWidth || window.innerWidth) / baseViewport.width;
				const viewport = page.getViewport({ scale });

				const canvas = document.createElement('canvas');
				canvas.width = viewport.width;
				canvas.height = viewport.height;
				canvas.style.cssText = 'display:block;width:100%;margin-bottom:8px';
				canvasContainer.appendChild(canvas);

				const ctx = canvas.getContext('2d');
				if (!ctx) continue;
				await page.render({ canvasContext: ctx, viewport, canvas }).promise;
			}

			loaded = true;
		} catch {
			pdfError = true;
		}
	}
</script>

<div class="original-panel">
	{#if !loaded && !pdfError}
		<div class="pdf-loading"><div class="pdf-spinner"></div></div>
	{/if}
	{#if isMobile}
		{#if pdfError}
			<div class="pdf-error">Failed to render PDF.</div>
		{/if}
		<div class="canvas-container" bind:this={canvasContainer} class:canvas-hidden={!loaded}></div>
	{:else}
		<iframe {src} title="Original file" class="original-iframe"
			class:iframe-hidden={!loaded}
			on:load={() => loaded = true}></iframe>
	{/if}
</div>

<style>
	.original-panel {
		flex: 1;
		overflow: auto;
		display: flex;
		flex-direction: column;
		background: var(--bg);
	}

	.original-iframe {
		flex: 1;
		width: 100%;
		height: 100%;
		border: none;
		min-height: 400px;
	}

	.iframe-hidden {
		display: none;
	}

	.canvas-container {
		display: block;
		padding: 8px;
	}

	.canvas-hidden {
		display: none;
	}

	.pdf-loading {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.pdf-spinner {
		width: 32px;
		height: 32px;
		border: 3px solid rgba(255, 255, 255, 0.08);
		border-top-color: var(--accent, #58a6ff);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	.pdf-error {
		padding: 24px;
		color: var(--text-muted);
		text-align: center;
		font-size: 13px;
	}

	@keyframes spin {
		to { transform: rotate(360deg); }
	}
</style>

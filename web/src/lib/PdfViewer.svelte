<script lang="ts">
	import { onMount } from 'svelte';
	import type { PDFDocumentProxy } from 'pdfjs-dist';
	import {
		isAndroidUserAgent,
		resolveAvailableWidth,
		computeRenderScale,
		RenderGuard,
		clampZoom,
		ZOOM_STEP,
		ZOOM_MIN,
		ZOOM_MAX
	} from './pdfViewerLogic';

	// How long to wait after the last zoom click before re-rendering at native
	// resolution. Keeps rapid taps snappy (CSS-only) while still sharpening once
	// the user settles on a zoom level.
	const ZOOM_SHARPEN_DEBOUNCE_MS = 400;

	export let src: string;

	// Android Chrome does not render PDFs inline in iframes — use PDF.js there.
	const isMobile = typeof navigator !== 'undefined' && isAndroidUserAgent(navigator.userAgent);

	let loaded = false;
	let pdfError = false;
	let canvasContainer: HTMLDivElement;
	let renderedSrc = '';
	let pdfDoc: PDFDocumentProxy | null = null;
	let zoomLevel = 1;
	const renderGuard = new RenderGuard();
	let sharpenTimer: ReturnType<typeof setTimeout> | null = null;

	// Reset spinner when src changes (iframe path only — mobile reset happens in renderPdf).
	$: if (!isMobile) { src; loaded = false; }

	// Re-render when src changes after the component is mounted (canvasContainer guard
	// prevents this running before bind:this is resolved on the initial mount pass).
	$: if (isMobile && src !== renderedSrc && canvasContainer) {
		renderPdf(src);
	}

	onMount(() => {
		if (isMobile) renderPdf(src);
		return () => {
			if (sharpenTimer) clearTimeout(sharpenTimer);
		};
	});

	async function renderPdf(forSrc: string) {
		const token = renderGuard.start();
		renderedSrc = forSrc;
		loaded = false;
		pdfError = false;
		pdfDoc = null;
		zoomLevel = 1;
		if (sharpenTimer) {
			clearTimeout(sharpenTimer);
			sharpenTimer = null;
		}

		try {
			// Dynamic import — only fetched the first time a PDF is viewed on mobile.
			const pdfjsLib = await import('pdfjs-dist');
			if (!renderGuard.isCurrent(token)) return;
			pdfjsLib.GlobalWorkerOptions.workerSrc = new URL(
				'pdfjs-dist/build/pdf.worker.mjs',
				import.meta.url
			).href;

			const pdf = await pdfjsLib.getDocument({ url: forSrc }).promise;
			if (!renderGuard.isCurrent(token)) return;
			pdfDoc = pdf;

			await renderPages(token);
			if (renderGuard.isCurrent(token)) loaded = true;
		} catch (err) {
			if (!renderGuard.isCurrent(token)) return;
			console.error('Failed to render PDF:', err);
			pdfError = true;
		}
	}

	// Draws every page of `pdfDoc` at the current `zoomLevel`. Runs in the background —
	// nothing in the UI ever waits on it, so the zoom buttons can't get stuck on it.
	async function renderPages(token: number) {
		if (!renderGuard.isCurrent(token) || !canvasContainer || !pdfDoc) return;
		canvasContainer.innerHTML = '';

		const availableWidth = resolveAvailableWidth(
			canvasContainer.parentElement?.clientWidth ?? 0,
			window.innerWidth
		);

		for (let pageNum = 1; pageNum <= pdfDoc.numPages; pageNum++) {
			const page = await pdfDoc.getPage(pageNum);
			if (!renderGuard.isCurrent(token)) return;
			const baseViewport = page.getViewport({ scale: 1 });
			const scale = computeRenderScale(baseViewport.width, availableWidth) * zoomLevel;
			const viewport = page.getViewport({ scale });

			const canvas = document.createElement('canvas');
			canvas.width = viewport.width;
			canvas.height = viewport.height;
			canvas.style.cssText = `display:block;width:${zoomLevel * 100}%;margin-bottom:8px`;
			canvasContainer.appendChild(canvas);

			const ctx = canvas.getContext('2d');
			if (!ctx) continue;
			await page.render({ canvasContext: ctx, viewport, canvas }).promise;
			if (!renderGuard.isCurrent(token)) return;
		}
	}

	// Instant feedback: CSS-resize the canvases already on screen. Then, once the user
	// stops clicking for a moment, re-render at native resolution so text stays sharp
	// instead of just being a blown-up/shrunk copy of the 100% raster.
	function setZoom(next: number) {
		zoomLevel = clampZoom(next);
		if (canvasContainer) {
			const width = `${zoomLevel * 100}%`;
			for (const child of canvasContainer.children) {
				(child as HTMLElement).style.width = width;
			}
		}

		if (sharpenTimer) clearTimeout(sharpenTimer);
		sharpenTimer = setTimeout(() => {
			sharpenTimer = null;
			sharpenAtCurrentZoom();
		}, ZOOM_SHARPEN_DEBOUNCE_MS);
	}

	async function sharpenAtCurrentZoom() {
		if (!pdfDoc) return;
		const token = renderGuard.start();
		try {
			await renderPages(token);
		} catch (err) {
			if (!renderGuard.isCurrent(token)) return;
			console.error('Failed to sharpen PDF at new zoom:', err);
		}
	}

	function zoomIn() {
		setZoom(zoomLevel + ZOOM_STEP);
	}

	function zoomOut() {
		setZoom(zoomLevel - ZOOM_STEP);
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
		<div class="canvas-scroll">
			<div class="canvas-container" bind:this={canvasContainer} class:canvas-hidden={!loaded}></div>
		</div>
		{#if loaded && !pdfError}
			<div class="zoom-controls">
				<button
					class="zoom-btn"
					on:click={zoomOut}
					disabled={zoomLevel <= ZOOM_MIN}
					aria-label="Zoom out"
				>−</button>
				<span class="zoom-level">{Math.round(zoomLevel * 100)}%</span>
				<button
					class="zoom-btn"
					on:click={zoomIn}
					disabled={zoomLevel >= ZOOM_MAX}
					aria-label="Zoom in"
				>+</button>
			</div>
		{/if}
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
		position: relative;
	}

	.canvas-scroll {
		flex: 1;
		overflow: auto;
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

	.zoom-controls {
		position: absolute;
		top: 16px;
		left: 16px;
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 10px;
		background: rgba(0, 0, 0, 0.55);
		backdrop-filter: blur(4px);
		border-radius: 20px;
		z-index: 10;
	}

	.zoom-btn {
		width: 28px;
		height: 28px;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 16px;
		line-height: 1;
		background: rgba(255, 255, 255, 0.15);
		border: none;
		border-radius: 50%;
		color: #fff;
		cursor: pointer;
		padding: 0;
	}

	.zoom-btn:disabled {
		opacity: 0.4;
		cursor: default;
	}

	.zoom-level {
		min-width: 42px;
		text-align: center;
		font-size: 12px;
		color: #fff;
	}

	@keyframes spin {
		to { transform: rotate(360deg); }
	}
</style>

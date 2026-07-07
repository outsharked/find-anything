<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import IconFitViewport from '$lib/icons/IconFitViewport.svelte';
	import IconAdjust from '$lib/icons/IconAdjust.svelte';

	let { src, svgMode = false }: { src: string; svgMode?: boolean } = $props();

	let container: HTMLDivElement | undefined = $state();
	let img: HTMLImageElement | undefined = $state();

	let scale = $state(1);
	let offsetX = $state(0);
	let offsetY = $state(0);
	let fitScale = $state(1);

	let loaded = $state(false);
	let loadError = $state(false);
	let showSpinner = $state(false);
	let spinnerTimer: ReturnType<typeof setTimeout> | null = null;
	let activeSrc: string | undefined;

	function clearSpinnerTimer() {
		if (spinnerTimer !== null) { clearTimeout(spinnerTimer); spinnerTimer = null; }
	}

	// Reset loading state only when src actually changes value.
	// Delay the spinner by 1 s so fast/cached images don't flash.
	$effect(() => {
		if (src !== activeSrc) {
			activeSrc = src;
			loaded = false;
			loadError = false;
			showSpinner = false;
			clearSpinnerTimer();
			spinnerTimer = setTimeout(() => { if (!loaded) showSpinner = true; }, 1000);
		}
	});

	function onError() {
		clearSpinnerTimer();
		showSpinner = false;
		loadError = true;
		loaded = true;
	}

	let dragging = $state(false);
	let dragStartX = 0;
	let dragStartY = 0;
	let dragOriginX = 0;
	let dragOriginY = 0;

	// Image adjustments
	let adjustOpen = $state(false);
	let invert = $state(false);
	let flipH = $state(false);
	let flipV = $state(false);
	let brightness = $state(100);
	let contrast = $state(100);
	let rotation = $state(0); // degrees, multiples of 90

	let adjustActive = $derived(invert || flipH || flipV || brightness !== 100 || contrast !== 100 || rotation !== 0);

	let imgFilter = $derived([
		invert ? 'invert(1)' : '',
		brightness !== 100 ? `brightness(${brightness / 100})` : '',
		contrast !== 100 ? `contrast(${contrast / 100})` : '',
	]
		.filter(Boolean)
		.join(' '));

	$effect(() => {
		if (img) img.style.filter = imgFilter;
	});

	function resetAdjust() {
		invert = false;
		flipH = false;
		flipV = false;
		brightness = 100;
		contrast = 100;
		rotation = 0;
	}

	const MAX_SCALE = 10;

	function clamp(v: number, min: number, max: number) {
		return Math.max(min, Math.min(max, v));
	}

	function applyTransform() {
		if (img) {
			const sx = scale * (flipH ? -1 : 1);
			const sy = scale * (flipV ? -1 : 1);
			img.style.transform = `translate(${offsetX}px, ${offsetY}px) rotate(${rotation}deg) scale(${sx}, ${sy})`;
		}
	}

	function rotateLeft() {
		rotation = (rotation - 90 + 360) % 360;
		applyTransform();
	}

	function rotateRight() {
		rotation = (rotation + 90) % 360;
		applyTransform();
	}

	function onImageLoad() {
		clearSpinnerTimer();
		showSpinner = false;
		loaded = true;
		loadError = false;
		rotation = 0;
		if (svgMode) {
			scale = 1;
			fitScale = 1;
			offsetX = 0;
			offsetY = 0;
			applyTransform();
			return;
		}
		if (!container || !img) return;
		const vw = container.clientWidth;
		const vh = container.clientHeight;
		const nw = img.naturalWidth;
		const nh = img.naturalHeight;

		if (nw <= vw && nh <= vh) {
			fitScale = 1;
		} else {
			fitScale = Math.min(vw / nw, vh / nh);
		}
		scale = fitScale;
		offsetX = 0;
		offsetY = 0;
		applyTransform();
	}

	function onWheel(e: WheelEvent) {
		e.preventDefault();
		const delta = e.deltaY > 0 ? 0.9 : 1.1;
		scale = clamp(scale * delta, minScale, MAX_SCALE);
		applyTransform();
	}

	function onPointerDown(e: PointerEvent) {
		if (e.button !== 0) return;
		if ((e.target as Element).closest('.toolbar, .adjust-panel')) return;
		dragging = true;
		dragStartX = e.clientX;
		dragStartY = e.clientY;
		dragOriginX = offsetX;
		dragOriginY = offsetY;
		container?.setPointerCapture(e.pointerId);
	}

	function onPointerMove(e: PointerEvent) {
		if (!dragging) return;
		offsetX = dragOriginX + (e.clientX - dragStartX);
		offsetY = dragOriginY + (e.clientY - dragStartY);
		applyTransform();
	}

	function onPointerUp() {
		dragging = false;
	}

	function onDblClick(e: MouseEvent) {
		if ((e.target as Element).closest('.toolbar, .adjust-panel')) return;
		scale = fitScale;
		offsetX = 0;
		offsetY = 0;
		applyTransform();
	}

	// Re-apply transform when flip changes. applyTransform() also reads scale/
	// offset/rotation, which already call it explicitly at their own call sites
	// for immediate (non-batched) visual feedback during drag/zoom — this effect
	// re-running for those too is a harmless redundant reapplication of the same
	// values, not a behavior change.
	$effect(() => {
		if (img) applyTransform();
	});

	// Never zoom out below half of fitScale (or 0.01 before image loads).
	let minScale = $derived(Math.min(0.01, fitScale * 0.5));

	function zoomIn() {
		scale = clamp(scale * 1.25, minScale, MAX_SCALE);
		applyTransform();
	}

	function zoomOut() {
		scale = clamp(scale / 1.25, minScale, MAX_SCALE);
		applyTransform();
	}

	function reset() {
		scale = fitScale;
		offsetX = 0;
		offsetY = 0;
		applyTransform();
	}

	onMount(() => {
		if (!container || !img) return;
		container.addEventListener('wheel', onWheel, { passive: false });
		if (img.complete && img.naturalWidth > 0) onImageLoad();
		else if (img.complete && img.naturalWidth === 0) onError();
	});

	onDestroy(() => {
		if (container) container.removeEventListener('wheel', onWheel);
		clearSpinnerTimer();
	});
</script>

<div class="viewer-wrap">
	{#if loadError}
		<div class="img-error">Image could not be displayed. The source file may not be accessible — check your source path configuration.</div>
	{/if}
	<div
		class="container"
		class:dragging
		class:hidden={loadError}
		bind:this={container}
		onpointerdown={onPointerDown}
		onpointermove={onPointerMove}
		onpointerup={onPointerUp}
		onpointercancel={onPointerUp}
		ondblclick={onDblClick}
		role="img"
		aria-label="Image viewer"
	>
		{#if showSpinner}<div class="img-loading"><div class="img-spinner"></div></div>{/if}
		<img
			bind:this={img}
			{src}
			alt=""
			class:svg-fit={svgMode}
			onload={onImageLoad}
			onerror={onError}
			draggable="false"
		/>
		<div class="toolbar">
			<button onclick={(e) => { e.stopPropagation(); zoomIn(); }} title="Zoom in">+</button>
			<button onclick={(e) => { e.stopPropagation(); zoomOut(); }} title="Zoom out">−</button>
			<button onclick={(e) => { e.stopPropagation(); reset(); }} title="Fit to viewport">
				<IconFitViewport />
			</button>
			<button onclick={(e) => { e.stopPropagation(); rotateLeft(); }} title="Rotate left">↺</button>
			<button onclick={(e) => { e.stopPropagation(); rotateRight(); }} title="Rotate right">↻</button>
			<button
				onclick={(e) => { e.stopPropagation(); adjustOpen = !adjustOpen; }}
				title="Adjust image"
				class:active={adjustActive}
			>
				<IconAdjust />
			</button>
		</div>
		{#if adjustOpen}
			<div class="adjust-panel">
				<div class="adjust-toggles">
					<button
						class:on={invert}
						onclick={(e) => { e.stopPropagation(); invert = !invert; }}
						title="Invert colours"
					>Invert</button>
					<button
						class:on={flipH}
						onclick={(e) => { e.stopPropagation(); flipH = !flipH; }}
						title="Flip horizontal"
					>Flip H</button>
					<button
						class:on={flipV}
						onclick={(e) => { e.stopPropagation(); flipV = !flipV; }}
						title="Flip vertical"
					>Flip V</button>
				</div>
				<label class="adjust-slider">
					<span class="adjust-label">Brightness</span>
					<input
						type="range"
						min="0"
						max="200"
						bind:value={brightness}
						onclick={(e) => e.stopPropagation()}
					/>
					<span class="adjust-value">{brightness}%</span>
				</label>
				<label class="adjust-slider">
					<span class="adjust-label">Contrast</span>
					<input
						type="range"
						min="0"
						max="200"
						bind:value={contrast}
						onclick={(e) => e.stopPropagation()}
					/>
					<span class="adjust-value">{contrast}%</span>
				</label>
				{#if adjustActive}
					<button class="reset-btn" onclick={(e) => { e.stopPropagation(); resetAdjust(); }}>Reset</button>
				{/if}
			</div>
		{/if}
	</div>
</div>

<style>
	.viewer-wrap {
		flex: 1;
		display: flex;
		flex-direction: column;
		min-height: 0;
		overflow: hidden;
	}

	.img-loading {
		position: absolute;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 5;
	}

	.img-spinner {
		width: 32px;
		height: 32px;
		border: 3px solid rgba(255, 255, 255, 0.08);
		border-top-color: var(--accent, #58a6ff);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		to { transform: rotate(360deg); }
	}

	.img-error {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 24px;
		color: var(--fg-muted, rgba(255, 255, 255, 0.5));
		font-size: 13px;
		text-align: center;
	}

	.container.hidden {
		display: none;
	}

	.toolbar {
		position: absolute;
		top: 8px;
		left: 8px;
		display: flex;
		gap: 4px;
		z-index: 10;
		opacity: 0;
		transition: opacity 0.15s;
	}

	.container:hover .toolbar {
		opacity: 1;
	}

	.toolbar button {
		background: rgba(0, 0, 0, 0.45);
		border: 1px solid rgba(255, 255, 255, 0.15);
		color: #fff;
		padding: 2px 10px;
		border-radius: var(--radius);
		cursor: pointer;
		font-size: 14px;
		line-height: 1.4;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		backdrop-filter: blur(4px);
	}

	.toolbar button:hover {
		background: rgba(0, 0, 0, 0.65);
		border-color: rgba(255, 255, 255, 0.35);
	}

	.toolbar button.active {
		border-color: rgba(255, 200, 80, 0.7);
		color: #ffc850;
	}

	/* Adjust panel */
	.adjust-panel {
		position: absolute;
		top: 44px;
		left: 8px;
		z-index: 10;
		background: rgba(0, 0, 0, 0.75);
		border: 1px solid rgba(255, 255, 255, 0.15);
		border-radius: var(--radius);
		backdrop-filter: blur(8px);
		padding: 10px 12px;
		display: flex;
		flex-direction: column;
		gap: 8px;
		min-width: 200px;
		color: #fff;
		font-size: 12px;
	}

	.adjust-toggles {
		display: flex;
		gap: 6px;
	}

	.adjust-toggles button {
		flex: 1;
		background: rgba(255, 255, 255, 0.08);
		border: 1px solid rgba(255, 255, 255, 0.15);
		color: #fff;
		padding: 4px 6px;
		border-radius: var(--radius);
		cursor: pointer;
		font-size: 11px;
		text-align: center;
	}

	.adjust-toggles button:hover {
		background: rgba(255, 255, 255, 0.18);
	}

	.adjust-toggles button.on {
		background: rgba(255, 200, 80, 0.2);
		border-color: rgba(255, 200, 80, 0.7);
		color: #ffc850;
	}

	.adjust-slider {
		display: grid;
		grid-template-columns: 72px 1fr 36px;
		align-items: center;
		gap: 8px;
	}

	.adjust-label {
		color: rgba(255, 255, 255, 0.7);
		font-size: 11px;
	}

	.adjust-value {
		color: rgba(255, 255, 255, 0.7);
		font-size: 11px;
		text-align: right;
	}

	input[type='range'] {
		width: 100%;
		accent-color: #ffc850;
		cursor: pointer;
	}

	.reset-btn {
		background: rgba(255, 255, 255, 0.08);
		border: 1px solid rgba(255, 255, 255, 0.15);
		color: rgba(255, 255, 255, 0.6);
		padding: 3px 8px;
		border-radius: var(--radius);
		cursor: pointer;
		font-size: 11px;
		align-self: flex-end;
	}

	.reset-btn:hover {
		background: rgba(255, 255, 255, 0.18);
		color: #fff;
	}

	.container {
		flex: 1;
		overflow: hidden;
		position: relative;
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: grab;
		user-select: none;
		background: var(--bg);
	}

	.container.dragging {
		cursor: grabbing;
	}

	img {
		position: absolute;
		transform-origin: center center;
		max-width: none;
		max-height: none;
		display: block;
		pointer-events: none;
	}

	img.svg-fit {
		width: 100%;
		height: 100%;
		object-fit: contain;
	}

	@media (max-width: 768px) {
		.viewer-wrap { flex: none; }
		.container { height: 60vh; max-height: 60vh; flex: none; }
	}
</style>

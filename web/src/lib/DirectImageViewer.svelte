<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import IconFitViewport from '$lib/icons/IconFitViewport.svelte';

	export let src: string;
	export let svgMode = false;

	let container: HTMLDivElement;
	let img: HTMLImageElement;

	let scale = 1;
	let offsetX = 0;
	let offsetY = 0;
	let fitScale = 1;

	let dragging = false;
	let dragStartX = 0;
	let dragStartY = 0;
	let dragOriginX = 0;
	let dragOriginY = 0;

	const MAX_SCALE = 10;

	function clamp(v: number, min: number, max: number) {
		return Math.max(min, Math.min(max, v));
	}

	function applyTransform() {
		if (img) {
			img.style.transform = `translate(${offsetX}px, ${offsetY}px) scale(${scale})`;
		}
	}

	function onImageLoad() {
		if (svgMode) {
			// SVGs are vector-based; CSS handles fit via width/height 100%.
			// JS fit calculation is skipped — fitScale stays 1.
			scale = 1;
			fitScale = 1;
			offsetX = 0;
			offsetY = 0;
			applyTransform();
			return;
		}
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
		if ((e.target as Element).closest('.toolbar')) return;
		dragging = true;
		dragStartX = e.clientX;
		dragStartY = e.clientY;
		dragOriginX = offsetX;
		dragOriginY = offsetY;
		container.setPointerCapture(e.pointerId);
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
		if ((e.target as Element).closest('.toolbar')) return;
		scale = fitScale;
		offsetX = 0;
		offsetY = 0;
		applyTransform();
	}

	// Never zoom out below half of fitScale (or 0.01 before image loads).
	$: minScale = Math.min(0.01, fitScale * 0.5);

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
		container.addEventListener('wheel', onWheel, { passive: false });
		if (img.complete) onImageLoad();
	});

	onDestroy(() => {
		if (container) container.removeEventListener('wheel', onWheel);
	});
</script>

<div class="viewer-wrap">
	<div
		class="container"
		class:dragging
		bind:this={container}
		on:pointerdown={onPointerDown}
		on:pointermove={onPointerMove}
		on:pointerup={onPointerUp}
		on:pointercancel={onPointerUp}
		on:dblclick={onDblClick}
		role="img"
		aria-label="Image viewer"
	>
		<img
			bind:this={img}
			{src}
			alt=""
			class:svg-fit={svgMode}
			on:load={onImageLoad}
			draggable="false"
		/>
		<div class="toolbar">
			<button on:click|stopPropagation={zoomIn} title="Zoom in">+</button>
			<button on:click|stopPropagation={zoomOut} title="Zoom out">−</button>
			<button on:click|stopPropagation={reset} title="Fit to viewport">
				<IconFitViewport />
			</button>
		</div>
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

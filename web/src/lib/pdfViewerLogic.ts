// .canvas-container has 8px padding on each side (see PdfViewer.svelte styles) —
// subtract it so the rendered canvas resolution matches its actual content width.
const CANVAS_CONTAINER_PADDING = 16;

export function isAndroidUserAgent(userAgent: string): boolean {
	return /Android/i.test(userAgent);
}

/**
 * The canvas container is hidden (display:none) for the whole render pass, so its own
 * clientWidth always reads 0. Use the (always-visible) parent's width instead, falling
 * back to the window width if the parent hasn't been measured either.
 */
export function resolveAvailableWidth(parentClientWidth: number, windowInnerWidth: number): number {
	const base = parentClientWidth > 0 ? parentClientWidth : windowInnerWidth;
	return Math.max(base - CANVAS_CONTAINER_PADDING, 1);
}

export function computeRenderScale(pageWidth: number, availableWidth: number): number {
	return availableWidth / pageWidth;
}

export const ZOOM_MIN = 0.5;
export const ZOOM_MAX = 3;
export const ZOOM_STEP = 0.25;

export function clampZoom(zoom: number): number {
	return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, zoom));
}

/**
 * Tracks which render request is the current one, so an in-flight async render that has
 * been superseded by a newer `src` can detect it's stale and bail out instead of writing
 * over the newer render's DOM/state.
 */
export class RenderGuard {
	private token = 0;

	start(): number {
		return ++this.token;
	}

	isCurrent(token: number): boolean {
		return token === this.token;
	}
}

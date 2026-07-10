/**
 * Computes the scale factor DirectImageViewer's "Fit to viewport" action applies:
 * scale-to-contain, filling as much of the container as possible on the binding
 * axis while preserving aspect ratio. Upscales small images as well as
 * downscaling large ones — "fit" always fills the viewport.
 */
export function computeFitScale(
	containerWidth: number,
	containerHeight: number,
	naturalWidth: number,
	naturalHeight: number
): number {
	if (!naturalWidth || !naturalHeight || !containerWidth || !containerHeight) return 1;
	return Math.min(containerWidth / naturalWidth, containerHeight / naturalHeight);
}

/**
 * Decides the default view when an image first opens (or view is reset):
 * fit-to-viewport (shrunk) only if the image actually overflows the
 * container in some dimension; otherwise native size (scale 1), so small
 * images aren't upscaled unless the user explicitly asks for it via the
 * fit toggle.
 */
export function shouldDefaultToFit(
	containerWidth: number,
	containerHeight: number,
	naturalWidth: number,
	naturalHeight: number
): boolean {
	if (!naturalWidth || !naturalHeight || !containerWidth || !containerHeight) return false;
	return naturalWidth > containerWidth || naturalHeight > containerHeight;
}

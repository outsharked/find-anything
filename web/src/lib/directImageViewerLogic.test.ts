import { describe, it, expect } from 'vitest';
import { computeFitScale, shouldDefaultToFit } from './directImageViewerLogic';

describe('computeFitScale', () => {
	it('downscales an image larger than the container in both dimensions', () => {
		// 4000x3000 image in an 800x600 viewport — should shrink by 0.2x.
		expect(computeFitScale(800, 600, 4000, 3000)).toBeCloseTo(0.2);
	});

	it('downscales using the more constraining dimension', () => {
		// Wide image, narrow container: width is the binding constraint.
		expect(computeFitScale(400, 1000, 2000, 1000)).toBeCloseTo(0.2);
	});

	it('upscales an image smaller than the container to fill it', () => {
		// A 93 MB Nikon D850 .NEF preview is often a small embedded JPEG
		// thumbnail (e.g. 300x225) — much smaller than a typical viewer panel.
		// "Fit to viewport" should fill the available space in both directions,
		// not leave the image at native size just because it's already small.
		expect(computeFitScale(1200, 800, 300, 225)).toBeCloseTo(800 / 225);
	});

	it('upscales using the more constraining dimension', () => {
		// Tall narrow image in a wide container: height is the binding constraint.
		expect(computeFitScale(1000, 400, 100, 200)).toBeCloseTo(2);
	});

	it('returns 1 for a zero-sized natural image (not yet loaded)', () => {
		expect(computeFitScale(800, 600, 0, 0)).toBe(1);
	});

	it('returns 1 for a zero-sized container (not yet measured/hidden)', () => {
		expect(computeFitScale(0, 0, 800, 600)).toBe(1);
	});

	it('returns 1 when the image exactly matches the container size', () => {
		expect(computeFitScale(800, 600, 800, 600)).toBe(1);
	});
});

describe('shouldDefaultToFit', () => {
	it('defaults to fit when the image overflows both dimensions', () => {
		expect(shouldDefaultToFit(800, 600, 4000, 3000)).toBe(true);
	});

	it('defaults to fit when the image overflows only one dimension', () => {
		expect(shouldDefaultToFit(800, 600, 4000, 300)).toBe(true);
		expect(shouldDefaultToFit(800, 600, 300, 3000)).toBe(true);
	});

	it('defaults to native size when the image is smaller than the container in both dimensions', () => {
		// The small D850 .NEF thumbnail case: don't upscale by default.
		expect(shouldDefaultToFit(1200, 800, 300, 225)).toBe(false);
	});

	it('defaults to native size when the image exactly matches the container', () => {
		expect(shouldDefaultToFit(800, 600, 800, 600)).toBe(false);
	});

	it('defaults to native size for a zero-sized natural image (not yet loaded)', () => {
		expect(shouldDefaultToFit(800, 600, 0, 0)).toBe(false);
	});

	it('defaults to native size for a zero-sized container (not yet measured/hidden)', () => {
		expect(shouldDefaultToFit(0, 0, 800, 600)).toBe(false);
	});
});

import { describe, it, expect } from 'vitest';
import {
	isAndroidUserAgent,
	resolveAvailableWidth,
	computeRenderScale,
	RenderGuard,
	clampZoom,
	ZOOM_MIN,
	ZOOM_MAX
} from './pdfViewerLogic';

describe('isAndroidUserAgent', () => {
	it('matches Android Chrome user agents', () => {
		expect(
			isAndroidUserAgent(
				'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Chrome/120.0.0.0 Mobile Safari/537.36'
			)
		).toBe(true);
	});

	it('is case-insensitive', () => {
		expect(isAndroidUserAgent('...android...')).toBe(true);
	});

	it('does not match iOS or desktop user agents', () => {
		expect(isAndroidUserAgent('Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)')).toBe(false);
		expect(isAndroidUserAgent('Mozilla/5.0 (Windows NT 10.0; Win64; x64)')).toBe(false);
	});
});

describe('resolveAvailableWidth', () => {
	it('uses the parent width minus padding when the parent has been measured', () => {
		expect(resolveAvailableWidth(400, 800)).toBe(400 - 16);
	});

	it('falls back to window width minus padding when parent width is 0 (hidden container)', () => {
		expect(resolveAvailableWidth(0, 800)).toBe(800 - 16);
	});

	it('never returns a non-positive width', () => {
		expect(resolveAvailableWidth(0, 10)).toBe(1);
	});
});

describe('computeRenderScale', () => {
	it('scales a page to fill the available width', () => {
		expect(computeRenderScale(200, 400)).toBe(2);
	});
});

describe('clampZoom', () => {
	it('passes through values within range', () => {
		expect(clampZoom(1.5)).toBe(1.5);
	});

	it('clamps below the minimum', () => {
		expect(clampZoom(0.1)).toBe(ZOOM_MIN);
	});

	it('clamps above the maximum', () => {
		expect(clampZoom(10)).toBe(ZOOM_MAX);
	});
});

describe('RenderGuard', () => {
	it('only the most recently started token is current', () => {
		const guard = new RenderGuard();
		const first = guard.start();
		const second = guard.start();

		expect(guard.isCurrent(first)).toBe(false);
		expect(guard.isCurrent(second)).toBe(true);
	});

	it('a single started token is current until superseded', () => {
		const guard = new RenderGuard();
		const token = guard.start();
		expect(guard.isCurrent(token)).toBe(true);
	});
});

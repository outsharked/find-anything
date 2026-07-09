import { describe, it, expect } from 'vitest';
import { eventMatchesPrefix, computeRefreshWait } from './treeRowLogic';

describe('eventMatchesPrefix', () => {
	it('matches a direct child', () => {
		expect(eventMatchesPrefix({ path: 'docs/plans/foo.md' }, 'docs/plans/')).toBe(true);
	});

	it('matches a deeply nested descendant (ancestor check, not just parent)', () => {
		expect(eventMatchesPrefix({ path: 'docs/plans/sub/foo.md' }, 'docs/')).toBe(true);
	});

	it('does not match an unrelated path', () => {
		expect(eventMatchesPrefix({ path: 'src/lib/foo.ts' }, 'docs/')).toBe(false);
	});

	it('matches via new_path on a rename even when the old path does not match', () => {
		expect(eventMatchesPrefix({ path: 'src/foo.ts', new_path: 'docs/foo.ts' }, 'docs/')).toBe(true);
	});

	it('ignores a null new_path', () => {
		expect(eventMatchesPrefix({ path: 'src/foo.ts', new_path: null }, 'docs/')).toBe(false);
	});
});

describe('computeRefreshWait', () => {
	it('returns 0 when idle (no prior refresh this session)', () => {
		expect(computeRefreshWait(0, 100_000, 1000)).toBe(0);
	});

	it('returns 0 once the interval has fully elapsed', () => {
		expect(computeRefreshWait(1000, 2000, 1000)).toBe(0);
	});

	it('returns the remaining time mid-interval, capping refresh rate during a burst', () => {
		expect(computeRefreshWait(1000, 1400, 1000)).toBe(600);
	});

	it('never returns a negative wait', () => {
		expect(computeRefreshWait(1000, 5000, 1000)).toBe(0);
	});
});

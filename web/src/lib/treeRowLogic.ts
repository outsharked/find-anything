/**
 * Whether a live index event under `prefix` should trigger a refresh of that
 * directory row. Matches ancestors, not just the immediate parent, so a new
 * subdirectory becomes visible in its parent's listing even though the SSE
 * path points deeper (e.g. "dir/newsubdir/file.txt" must refresh "dir/").
 */
export function eventMatchesPrefix(ev: { path: string; new_path?: string | null }, prefix: string): boolean {
	return ev.path.startsWith(prefix) || (!!ev.new_path && ev.new_path.startsWith(prefix));
}

/**
 * Trailing-edge throttle wait, in ms, before the next refresh may run.
 *
 * A plain debounce (reset the timer on every event) never fires while events
 * keep arriving faster than the interval — a bulk scan can emit one live
 * event per file, so a shallow ancestor row would see no updates until the
 * entire scan goes quiet. Throttling instead caps refreshes to at most once
 * per `intervalMs`, so the row keeps updating during a long, sustained burst.
 */
export function computeRefreshWait(lastRefreshAt: number, now: number, intervalMs: number): number {
	return Math.max(0, intervalMs - (now - lastRefreshAt));
}

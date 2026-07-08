---
name: verify
description: How to drive the running find-anything web UI for live verification (dev servers, auth token, Playwright quirks on this machine)
---

# Verifying find-anything live

## Handles

- A dev `find-server` usually already runs on **:8765** (`target/debug/find-server --config .config/server.toml`, cwd = repo root). Its bearer token is **`test`** (see `.config/server.toml`); data dir `~/.local/share/find-anything`, sources `projects` and `zips`.
- Vite dev server on **:5174** (`mise run dev` starts both) proxies `/api` → :8765 and hot-reloads `web/src` changes — no rebuild needed to verify UI edits.
- API smoke: `curl -H "Authorization: Bearer test" "http://localhost:8765/api/v1/sources"`.
- Find test files: `GET /api/v1/files?source=projects&q=<name>&limit=5`. Good large paged file: `home/jamiet/code/find-anything/Cargo.lock` (7481 lines). Small non-markdown file: `.../install.sh`. Note markdown files render as formatted view by default — no `.code-row` in the DOM.

## Driving the UI (Playwright, Python)

- Auth: `ctx.add_init_script("localStorage.setItem('find_token', 'test')")` before first navigation.
- Deep link format: `http://localhost:5174/?view=file&fsource=projects&path=<path>#L<line>` (no leading `/` on path).
- **Headless screenshots hang forever on this machine** (WSL2; both `page.screenshot` and raw CDP `Page.captureScreenshot` block "waiting for fonts"). Launch **headed under xvfb** instead: `xvfb-run -a python3 script.py` with `pw.chromium.launch(headless=False)` — screenshots then work normally.
- **Never `pkill -f <script name>` in the same Bash call that mentions the name elsewhere** — it matches the invoking shell's own command line and kills it (exit 144). Use a `[x]` bracket pattern *and* keep the kill in its own invocation.

## Flows worth driving for file-viewer changes

Open big file (windowed path: bounded `.code-row` count, `.spacer-row` present, scrollHeight ≈ totalLines × rowHeight) → scrollbar jump to middle/bottom (debounced refetch ~100ms; assert top visible line) → `#L` deep link + runtime `location.hash` edit + Ctrl+G dialog (jump paths) → word-wrap toggle (narrow viewport ≤700px needed for the toggle to appear on Cargo.lock — it's gated on horizontal overflow) → small file (legacy path, no spacers).

import { writable } from 'svelte/store';

/** Lines shown before and after each match in search result cards (server-configured). */
export const contextWindow = writable(1);

/** Maximum markdown file size (KB) the UI will render as formatted HTML (server-configured). */
export const maxMarkdownRenderKb = writable(512);

/** Maximum content lines per /api/v1/file request. 0 = no limit (server-configured). */
export const fileViewPageSize = writable(2000);

/** Number of spaces a tab character occupies in the file viewer (server-configured, user-overridable). */
export const tabWidth = writable(4);

/** Public base URL of the server (e.g. `https://find.example.com`). Used as origin for share links. */
export const publicUrl = writable<string | undefined>(undefined);

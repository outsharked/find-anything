import { writable } from 'svelte/store';

/** Lines shown before and after each match in search result cards (server-configured). */
export const contextWindow = writable(1);

/** Maximum markdown file size (KB) the UI will render as formatted HTML (server-configured). */
export const maxMarkdownRenderKb = writable(512);

/** Maximum content lines per /api/v1/file request. 0 = no limit (server-configured). */
export const fileViewPageSize = writable(2000);

/**
 * The line_number of the first content line (2 for new servers, 1 for old).
 * Used to compute display line number: display = line_number - (contentLineStart - 1).
 */
export const contentLineStart = writable(1);

/** Number of spaces a tab character occupies in the file viewer (server-configured, user-overridable). */
export const tabWidth = writable(4);

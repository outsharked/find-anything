import { writable } from 'svelte/store';

/** Path of the tree item currently selected by keyboard navigation. */
export const keyboardCursorPath = writable<string | null>(null);

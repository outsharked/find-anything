import { writable } from 'svelte/store';

export interface UserProfile {
	sidebarWidth?: number;
	wordWrap?: boolean;
	markdownFormat?: boolean;
	rtfFormat?: boolean;
	sourceRoots?: Record<string, string>;
	handlerInstalled?: boolean;
	contextWindow?: number;
	tabWidth?: number;
	theme?: 'dark' | 'light' | 'system';
}

const STORAGE_KEY = 'find-anything.profile';

function loadProfile(): UserProfile {
	if (typeof localStorage === 'undefined') return {};
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		return raw ? JSON.parse(raw) : {};
	} catch {
		return {};
	}
}

function createProfileStore() {
	const store = writable<UserProfile>(loadProfile());
	if (typeof localStorage !== 'undefined') {
		store.subscribe((value) => {
			localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
		});
	}
	return store;
}

export const profile = createProfileStore();

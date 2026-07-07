<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import type { Snippet } from 'svelte';
	import { profile } from '$lib/profile';

	let { children }: { children: Snippet } = $props();

	function resolveTheme(theme: string | undefined): 'dark' | 'light' {
		const t = theme ?? 'dark';
		if (t === 'system') return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
		return t as 'dark' | 'light';
	}

	onMount(() => {
		let mq: MediaQueryList | null = null;
		let mqHandler: (() => void) | null = null;

		const unsub = profile.subscribe((p) => {
			if (mq && mqHandler) mq.removeEventListener('change', mqHandler);
			mq = null; mqHandler = null;

			const theme = p.theme ?? 'dark';
			document.documentElement.setAttribute('data-theme', resolveTheme(theme));

			if (theme === 'system') {
				mq = window.matchMedia('(prefers-color-scheme: dark)');
				mqHandler = () => document.documentElement.setAttribute('data-theme', resolveTheme('system'));
				mq.addEventListener('change', mqHandler);
			}
		});

		return () => {
			unsub();
			if (mq && mqHandler) mq.removeEventListener('change', mqHandler);
		};
	});
</script>

{@render children()}

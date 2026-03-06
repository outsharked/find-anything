<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { profile } from '$lib/profile';

	// SvelteKit passes params to every layout/page component. Declare it to avoid
	// the runtime "unknown prop" warning. Assigned to _params to signal that it
	// is intentionally unused (access via $page.params if ever needed).
	export let params: Record<string, string>;
	const _params = params;

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

<slot />

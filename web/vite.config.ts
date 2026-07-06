import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		host: true,
		port: 5174,
		proxy: {
			'/api': {
				target: 'http://localhost:8765',
				changeOrigin: true,
			}
		},
		// Vite 5 checks query-param values against the FS allow list, which trips on
		// file paths in the app's own URL (e.g. ?path=home/user/...). Disable strict
		// mode since the dev server is local-only and all actual file serving goes
		// through the proxied /api routes, not Vite's static file handler.
		fs: { strict: false }
	},
	build: {
		// rtf.js is intentionally large (~2.2MB) and lazy-loaded on demand. Vite 8's
		// default Rolldown bundler reports the "chunk larger than 500kB" notice through
		// its own reporter rather than the rollupOptions.onwarn hook, so raise the limit
		// instead of trying to suppress that specific warning.
		chunkSizeWarningLimit: 2500,
		rollupOptions: {
			onwarn(warning, warn) {
				if (warning.code === 'CHUNK_TOO_LARGE' && warning.message.includes('rtf.js')) return;
				warn(warning);
			}
		}
	}
});

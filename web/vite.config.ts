import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		host: true,
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
		rollupOptions: {
			onwarn(warning, warn) {
				// rtf.js is intentionally large and lazy-loaded on demand — suppress the chunk size warning.
				if (warning.code === 'CHUNK_TOO_LARGE' && warning.message.includes('rtf.js')) return;
				warn(warning);
			}
		}
	}
});

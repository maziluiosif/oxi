import {defineConfig} from 'vite';
import {svelte} from '@sveltejs/vite-plugin-svelte';
import {oxiBridge} from './dev/bridge-plugin';

// Tauri serves the dev build from a fixed port and expects it to fail rather than move.
export default defineConfig({
	plugins: [svelte(), oxiBridge()],
	clearScreen: false,
	server: {port: 1420, strictPort: true, watch: {ignored: ['**/src-tauri/**']}},
	build: {
		// ES2022: main.ts uses top-level await.
		target: 'es2022',
		chunkSizeWarningLimit: 8000,
		sourcemap: false,
	},
	worker: {format: 'es'},
});

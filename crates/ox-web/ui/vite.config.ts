import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		proxy: {
			'/complete': 'http://localhost:3000',
			'/pkg': 'http://localhost:3000',
		},
	},
});

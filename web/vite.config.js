import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    // dev proxy: forward API + media to the backend so `npm run dev` works
    // against a running `cargo run` instance.
    proxy: {
      '/v1': 'http://localhost:8099',
      '/v1.1': 'http://localhost:8099',
      '/v1.4': 'http://localhost:8099',
      '/v2': 'http://localhost:8099',
      '/connectdata': 'http://localhost:8099',
      '/health': 'http://localhost:8099',
    },
  },
});

import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Construit dans `dist/`, embarqué ensuite dans le binaire du master (rust-embed).
// `npm run dev` proxifie l'API vers un master local (127.0.0.1:7080).
export default defineConfig({
  plugins: [svelte()],
  server: { proxy: { '/api': 'http://127.0.0.1:7080' } },
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false, assetsInlineLimit: 0 },
});

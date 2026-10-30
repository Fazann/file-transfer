import { fileURLToPath, URL } from 'node:url';

import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

// Tauri expects a fixed port in development and fails if it is taken.
export default defineConfig({
  plugins: [svelte()],
  resolve: {
    alias: { $lib: fileURLToPath(new URL('./src/lib', import.meta.url)) },
    // Vitest runs in Node; this makes Svelte resolve its browser build, as in the app.
    conditions: process.env.VITEST ? ['browser'] : [],
  },
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: { target: 'safari15', outDir: 'dist' },
  test: { environment: 'jsdom', include: ['src/**/*.test.ts'] },
});

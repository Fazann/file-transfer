import { fileURLToPath, URL } from 'node:url';

import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

// Tauri expects a fixed port in development and fails if it is taken.
export default defineConfig({
  plugins: [svelte()],
  resolve: {
    alias: { $lib: fileURLToPath(new URL('./src/lib', import.meta.url)) },
    // Vitest runs in Node; this makes Svelte resolve its browser build, as in the app. Only set it there: any value
    // here replaces Vite's defaults, and without `browser` the app gets Svelte's server build and stays blank.
    ...(process.env.VITEST ? { conditions: ['browser'] } : {}),
  },
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: { target: 'safari15', outDir: 'dist' },
  test: { environment: 'jsdom', include: ['src/**/*.test.ts'] },
});

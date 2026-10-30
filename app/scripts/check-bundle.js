// Fails the build if the bundle holds Svelte's server code. Such a bundle passes every test, but the window stays blank.

import { readdirSync, readFileSync } from 'node:fs';

const SERVER_ONLY_ERROR = 'lifecycle_function_unavailable';

const assets = new URL('../dist/assets/', import.meta.url);
const bad = readdirSync(assets).filter(
  (name) => name.endsWith('.js') && readFileSync(new URL(name, assets), 'utf8').includes(SERVER_ONLY_ERROR),
);
if (bad.length > 0) {
  console.error(`Svelte's server build ended up in ${bad.join(', ')}. Check resolve.conditions in vite.config.ts.`);
  process.exit(1);
}

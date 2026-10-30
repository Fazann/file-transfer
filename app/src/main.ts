import { mount } from 'svelte';

// Only the scripts the UI uses, bundled with the app: no fonts are loaded from the network.

import '@fontsource/noto-sans/latin-400.css';
import '@fontsource/noto-sans/latin-600.css';
import '@fontsource/noto-sans-arabic/arabic-400.css';
import '@fontsource/noto-sans-arabic/arabic-600.css';
import '@fontsource/noto-sans-khmer/khmer-400.css';
import '@fontsource/noto-sans-khmer/khmer-600.css';

import { applyDocumentLanguage, setupI18n } from '$lib/i18n';

import App from './App.svelte';
import './app.css';

// Start in the system language; App switches to the saved one once settings are loaded.
applyDocumentLanguage(setupI18n(null));

const target = document.getElementById('app');
if (target === null) {
  throw new Error('missing #app element');
}

export default mount(App, { target });

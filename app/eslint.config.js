import js from '@eslint/js';
import prettier from 'eslint-config-prettier';
import importX from 'eslint-plugin-import-x';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';

export default ts.config(
  { ignores: ['dist/', 'node_modules/', 'src-tauri/'] },
  js.configs.recommended,
  ...ts.configs.strict,
  ...svelte.configs['flat/recommended'],
  {
    languageOptions: { globals: { ...globals.browser, ...globals.node } },
    plugins: { 'import-x': importX },
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      'no-restricted-imports': [
        'error',
        { paths: [{ name: '@tauri-apps/api/core', message: 'Use $lib/api instead.' }] },
      ],
      'import-x/order': [
        'error',
        {
          groups: ['builtin', 'external', 'internal', ['parent', 'sibling', 'index'], 'type'],
          pathGroups: [{ pattern: '$lib/**', group: 'internal' }],
          // Without this, `$lib/...` counts as an external package and the path group above never applies.
          pathGroupsExcludedImportTypes: ['builtin', 'type'],
          'newlines-between': 'always',
          alphabetize: { order: 'asc', caseInsensitive: true },
        },
      ],
    },
  },
  {
    files: ['**/*.svelte', '**/*.svelte.ts'],
    languageOptions: { parserOptions: { parser: ts.parser, extraFileExtensions: ['.svelte'] } },
  },
  { files: ['src/lib/api/**'], rules: { 'no-restricted-imports': 'off' } },
  // Must stay last: turns off every rule that Prettier handles.
  prettier,
);

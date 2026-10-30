<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    variant?: 'primary' | 'secondary' | 'danger' | 'ghost';
    type?: 'button' | 'submit';
    disabled?: boolean;
    onclick?: () => void;
    children: Snippet;
  }

  let { variant = 'secondary', type = 'button', disabled = false, onclick, children }: Props = $props();
</script>

<button class="button {variant}" {type} {disabled} {onclick}>
  {@render children()}
</button>

<style>
  .button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    min-block-size: 2.25rem;
    padding-block: var(--space-1);
    padding-inline: var(--space-4);
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    transition: background-color 120ms;
  }

  .button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .button:focus-visible {
    outline: 2px solid var(--color-focus);
    outline-offset: 2px;
  }

  .primary {
    background: var(--color-accent);
    color: var(--color-on-accent);
  }

  .primary:hover:not(:disabled) {
    background: var(--color-accent-strong);
  }

  .secondary {
    background: var(--color-surface);
    border-color: var(--color-border);
    color: var(--color-text);
  }

  .secondary:hover:not(:disabled) {
    background: var(--color-surface-hover);
  }

  .danger {
    background: var(--color-danger);
    color: var(--color-on-accent);
  }

  .ghost {
    background: transparent;
    color: var(--color-text);
  }

  .ghost:hover:not(:disabled) {
    background: var(--color-surface-hover);
  }
</style>

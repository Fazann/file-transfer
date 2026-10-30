<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    /** Outlines the card, e.g. while files are dragged over it. */
    highlighted?: boolean;
    children: Snippet;
  }

  let { highlighted = false, children }: Props = $props();
</script>

<div class="card" class:highlighted>
  {@render children()}
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-inline-size: 0;
    padding: var(--space-4);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-lg);
    background: var(--color-surface);
    transition:
      border-color 120ms,
      box-shadow 120ms;
  }

  .highlighted {
    border-color: var(--color-accent);
    box-shadow: 0 0 0 3px var(--color-accent-soft);
  }
</style>

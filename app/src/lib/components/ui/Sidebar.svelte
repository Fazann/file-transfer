<script lang="ts">
  import IconButton from './IconButton.svelte';

  import type { Snippet } from 'svelte';

  interface Props {
    collapsed: boolean;
    collapseLabel: string;
    expandLabel: string;
    header?: Snippet;
    children: Snippet;
  }

  let { collapsed = $bindable(), collapseLabel, expandLabel, header, children }: Props = $props();
</script>

<!-- Sits at the start edge: left in LTR, right in RTL, through flexbox order alone. -->
<nav class="sidebar" class:collapsed>
  <div class="top">
    {#if !collapsed}
      {@render header?.()}
    {/if}
    <IconButton
      icon={collapsed ? 'chevron-end' : 'chevron-start'}
      label={collapsed ? expandLabel : collapseLabel}
      onclick={() => (collapsed = !collapsed)}
    />
  </div>
  <ul>
    {@render children()}
  </ul>
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    inline-size: 13rem;
    padding: var(--space-2);
    border-inline-end: 1px solid var(--color-border);
    background: var(--color-surface);
    transition: inline-size 160ms ease;
  }

  .collapsed {
    inline-size: 3.5rem;
  }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    min-block-size: 2.5rem;
    padding-inline-start: var(--space-2);
  }

  .collapsed .top {
    justify-content: center;
    padding-inline-start: 0;
  }

  ul {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
</style>

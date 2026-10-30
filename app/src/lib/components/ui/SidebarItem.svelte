<script lang="ts">
  import Icon, { type IconName } from './Icon.svelte';

  interface Props {
    icon: IconName;
    label: string;
    active?: boolean;
    collapsed?: boolean;
    /** A count shown next to the label, e.g. running transfers. Hidden when 0. */
    count?: number;
    onclick: () => void;
  }

  let { icon, label, active = false, collapsed = false, count = 0, onclick }: Props = $props();
</script>

<li>
  <button
    class="item"
    class:active
    class:collapsed
    type="button"
    aria-current={active ? 'page' : undefined}
    title={collapsed ? label : undefined}
    {onclick}
  >
    <Icon name={icon} />
    {#if !collapsed}
      <span class="label">{label}</span>
    {/if}
    {#if count > 0}
      <span class="count">{count}</span>
    {/if}
  </button>
</li>

<style>
  .item {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    inline-size: 100%;
    min-block-size: 2.5rem;
    padding-inline: var(--space-3);
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--color-text-muted);
    font: inherit;
    text-align: start;
    cursor: pointer;
  }

  .item:hover {
    background: var(--color-surface-hover);
    color: var(--color-text);
  }

  .item:focus-visible {
    outline: 2px solid var(--color-focus);
  }

  .active {
    background: var(--color-accent-soft);
    color: var(--color-accent-strong);
    font-weight: 600;
  }

  .collapsed {
    justify-content: center;
    padding-inline: 0;
  }

  .label {
    flex: 1;
    min-inline-size: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .count {
    min-inline-size: 1.25rem;
    padding-inline: 0.35rem;
    border-radius: 999px;
    background: var(--color-accent);
    color: var(--color-on-accent);
    font-size: var(--font-size-sm);
    text-align: center;
  }

  .collapsed .count {
    position: absolute;
    inset-block-start: 0.1rem;
    inset-inline-end: 0.1rem;
  }
</style>

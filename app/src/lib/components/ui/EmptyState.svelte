<script lang="ts">
  import Icon, { type IconName } from './Icon.svelte';

  import type { Snippet } from 'svelte';

  interface Props {
    icon: IconName;
    title: string;
    description?: string;
    children?: Snippet;
  }

  let { icon, title, description, children }: Props = $props();
</script>

<div class="empty">
  <Icon name={icon} size={40} />
  <p class="title">{title}</p>
  {#if description}
    <p class="description">{description}</p>
  {/if}
  {@render children?.()}
</div>

<style>
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-2);
    padding-block: var(--space-8);
    color: var(--color-text-muted);
    text-align: center;
  }

  .title {
    margin: 0;
    color: var(--color-text);
    font-weight: 600;
  }

  .description {
    margin: 0;
    max-inline-size: 32rem;
  }
</style>

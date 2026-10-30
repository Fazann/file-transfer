<script lang="ts">
  import Icon, { type IconName } from './Icon.svelte';

  interface Props {
    icon: IconName;
    /** Read by screen readers and shown as a tooltip. */
    label: string;
    disabled?: boolean;
    onclick?: () => void;
  }

  let { icon, label, disabled = false, onclick }: Props = $props();
</script>

<button class="icon-button" type="button" aria-label={label} title={label} {disabled} {onclick}>
  <Icon name={icon} size={18} />
</button>

<style>
  .icon-button {
    display: inline-grid;
    place-items: center;
    inline-size: 2.25rem;
    block-size: 2.25rem;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--color-text-muted);
    cursor: pointer;
  }

  .icon-button:hover:not(:disabled) {
    background: var(--color-surface-hover);
    color: var(--color-text);
  }

  .icon-button:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .icon-button:focus-visible {
    outline: 2px solid var(--color-focus);
  }
</style>

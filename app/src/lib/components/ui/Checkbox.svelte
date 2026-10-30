<script lang="ts">
  interface Props {
    checked: boolean;
    label: string;
    description?: string;
    onchange?: (checked: boolean) => void;
  }

  let { checked = $bindable(), label, description, onchange }: Props = $props();
</script>

<label class="checkbox">
  <input type="checkbox" bind:checked onchange={() => onchange?.(checked)} />
  <span class="text">
    <span>{label}</span>
    {#if description}
      <span class="description">{description}</span>
    {/if}
  </span>
</label>

<style>
  .checkbox {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    cursor: pointer;
  }

  input {
    margin-block-start: 0.3rem;
    accent-color: var(--color-accent);
  }

  .text {
    display: flex;
    flex-direction: column;
    min-inline-size: 0;
  }

  .description {
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }
</style>

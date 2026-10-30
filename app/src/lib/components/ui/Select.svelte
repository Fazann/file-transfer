<script lang="ts">
  interface Option {
    value: string;
    label: string;
  }

  interface Props {
    value: string;
    label: string;
    id: string;
    options: Option[];
    onchange?: (value: string) => void;
  }

  let { value = $bindable(), label, id, options, onchange }: Props = $props();
</script>

<div class="field">
  <label for={id}>{label}</label>
  <select {id} bind:value onchange={() => onchange?.(value)}>
    {#each options as option (option.value)}
      <option value={option.value}>{option.label}</option>
    {/each}
  </select>
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  label {
    font-weight: 600;
  }

  select {
    min-block-size: 2.25rem;
    padding-inline: var(--space-3);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-background);
    color: var(--color-text);
    font: inherit;
  }

  select:focus-visible {
    outline: 2px solid var(--color-focus);
  }
</style>

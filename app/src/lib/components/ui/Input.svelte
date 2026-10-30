<script lang="ts">
  interface Props {
    value: string | number;
    label: string;
    id: string;
    type?: 'text' | 'number';
    hint?: string;
    min?: number;
    max?: number;
    /** `ltr` for paths, ports and similar values that read left to right in every language. */
    dir?: 'ltr' | 'auto';
    readonly?: boolean;
  }

  let {
    value = $bindable(),
    label,
    id,
    type = 'text',
    hint,
    min,
    max,
    dir = 'auto',
    readonly = false,
  }: Props = $props();
</script>

<div class="field">
  <label for={id}>{label}</label>
  <input {id} {type} {min} {max} {dir} {readonly} bind:value aria-describedby={hint ? `${id}-hint` : undefined} />
  {#if hint}
    <p class="hint" id="{id}-hint">{hint}</p>
  {/if}
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    min-inline-size: 0;
  }

  label {
    font-weight: 600;
  }

  input {
    min-block-size: 2.25rem;
    padding-inline: var(--space-3);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-background);
    color: var(--color-text);
    font: inherit;
  }

  input:focus-visible {
    outline: 2px solid var(--color-focus);
    outline-offset: 1px;
  }

  input[readonly] {
    color: var(--color-text-muted);
  }

  .hint {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }
</style>

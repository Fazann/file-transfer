<script lang="ts">
  interface Props {
    value: string;
    label: string;
    length?: number;
    disabled?: boolean;
    /** Called once every digit is filled. */
    oncomplete?: (pin: string) => void;
  }

  let { value = $bindable(), label, length = 6, disabled = false, oncomplete }: Props = $props();

  let inputs: HTMLInputElement[] = $state([]);
  const digits = $derived(Array.from({ length }, (_, index) => value[index] ?? ''));

  function setDigits(next: string, focusIndex: number): void {
    value = next.replace(/\D/g, '').slice(0, length);
    inputs[Math.min(focusIndex, length - 1)]?.focus();
    if (value.length === length) {
      oncomplete?.(value);
    }
  }

  function oninput(index: number, event: Event): void {
    const typed = (event.currentTarget as HTMLInputElement).value.replace(/\D/g, '');
    // Typing or pasting several digits fills the following boxes too.
    setDigits(value.slice(0, index) + typed + value.slice(index + typed.length), index + typed.length);
  }

  function onkeydown(index: number, event: KeyboardEvent): void {
    if (event.key === 'Backspace' && digits[index] === '' && index > 0) {
      setDigits(value.slice(0, index - 1), index - 1);
      event.preventDefault();
    }
  }
</script>

<!-- PINs read left to right in every language, so the boxes do too. -->
<div class="pin" dir="ltr" role="group" aria-label={label}>
  {#each digits as digit, index (index)}
    <input
      bind:this={inputs[index]}
      value={digit}
      inputmode="numeric"
      autocomplete="one-time-code"
      maxlength={length}
      aria-label="{label} {index + 1}"
      {disabled}
      oninput={(event) => oninput(index, event)}
      onkeydown={(event) => onkeydown(index, event)}
    />
  {/each}
</div>

<style>
  .pin {
    display: flex;
    gap: var(--space-2);
    justify-content: center;
  }

  input {
    inline-size: 2.75rem;
    block-size: 3.25rem;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-background);
    color: var(--color-text);
    font-size: 1.5rem;
    font-variant-numeric: tabular-nums;
    text-align: center;
  }

  input:focus-visible {
    outline: 2px solid var(--color-focus);
  }
</style>

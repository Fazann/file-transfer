<script lang="ts">
  import IconButton from './IconButton.svelte';

  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    closeLabel: string;
    onclose: () => void;
    children: Snippet;
    actions?: Snippet;
  }

  let { title, closeLabel, onclose, children, actions }: Props = $props();

  let dialog: HTMLDialogElement | undefined = $state();

  $effect(() => {
    dialog?.showModal();
  });
</script>

<!-- A native <dialog> traps focus and closes on Escape. -->
<dialog
  bind:this={dialog}
  aria-labelledby="modal-title"
  oncancel={(event) => {
    event.preventDefault();
    onclose();
  }}
>
  <header>
    <h2 id="modal-title">{title}</h2>
    <IconButton icon="close" label={closeLabel} onclick={onclose} />
  </header>
  <div class="body">
    {@render children()}
  </div>
  {#if actions}
    <footer>
      {@render actions()}
    </footer>
  {/if}
</dialog>

<style>
  dialog {
    inline-size: min(28rem, calc(100vw - 2rem));
    padding: 0;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-lg);
    background: var(--color-surface);
    color: var(--color-text);
    box-shadow: var(--shadow-lg);
  }

  dialog::backdrop {
    background: rgb(0 0 0 / 40%);
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    padding-block: var(--space-3);
    padding-inline: var(--space-4);
    border-block-end: 1px solid var(--color-border);
  }

  h2 {
    margin: 0;
    font-size: var(--font-size-lg);
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4);
  }

  footer {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
    padding-block: var(--space-3);
    padding-inline: var(--space-4);
    border-block-start: 1px solid var(--color-border);
  }
</style>

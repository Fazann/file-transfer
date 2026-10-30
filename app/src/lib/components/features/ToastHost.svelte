<script lang="ts">
  import { _ } from 'svelte-i18n';

  import Toast from '$lib/components/ui/Toast.svelte';
  import { dismissToast, toasts } from '$lib/stores/toasts';
</script>

<div class="host" aria-live="polite">
  {#each $toasts as toast (toast.id)}
    <Toast
      message={toast.text}
      tone={toast.tone}
      dismissLabel={$_('common.dismiss')}
      ondismiss={() => dismissToast(toast.id)}
    />
  {/each}
</div>

<style>
  .host {
    position: fixed;
    inset-block-end: var(--space-4);
    inset-inline-end: var(--space-4);
    z-index: 10;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    inline-size: min(24rem, calc(100vw - 2rem));
  }
</style>

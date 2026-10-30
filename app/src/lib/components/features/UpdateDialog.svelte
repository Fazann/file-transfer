<script lang="ts">
  import { _ } from 'svelte-i18n';

  import Button from '$lib/components/ui/Button.svelte';
  import Modal from '$lib/components/ui/Modal.svelte';
  import { showToast } from '$lib/stores/toasts';
  import { dismissUpdate, update } from '$lib/stores/update';

  let installing = $state(false);

  async function install(): Promise<void> {
    if ($update === null) {
      return;
    }
    installing = true;
    try {
      await $update.install();
    } catch {
      showToast($_('update.failed'), 'error');
      dismissUpdate();
    } finally {
      installing = false;
    }
  }
</script>

{#if $update}
  <Modal title={$_('update.title')} closeLabel={$_('update.later')} onclose={dismissUpdate}>
    <p>
      {installing
        ? $_('update.installing')
        : $_('update.body', { values: { version: $update.version, current: $update.currentVersion } })}
    </p>
    {#snippet actions()}
      <Button disabled={installing} onclick={dismissUpdate}>{$_('update.later')}</Button>
      <Button variant="primary" disabled={installing} onclick={() => void install()}>{$_('update.install')}</Button>
    {/snippet}
  </Modal>
{/if}

<style>
  p {
    margin: 0;
  }
</style>

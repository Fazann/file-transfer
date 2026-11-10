<script lang="ts">
  import { _ } from 'svelte-i18n';

  import { writeClipboard } from '$lib/api';
  import Button from '$lib/components/ui/Button.svelte';
  import Icon from '$lib/components/ui/Icon.svelte';
  import Modal from '$lib/components/ui/Modal.svelte';
  import { closeReceivedText, receivedTexts } from '$lib/stores/texts';
  import { showToast } from '$lib/stores/toasts';

  /** One text at a time, oldest first. */
  const received = $derived($receivedTexts[0]);
  const waiting = $derived(Math.max(0, $receivedTexts.length - 1));

  async function copy(text: string): Promise<void> {
    try {
      await writeClipboard(text);
      showToast($_('text.copied'), 'success');
    } catch {
      showToast($_('errors.IO'), 'error');
    }
  }
</script>

{#if received}
  <!-- `#key` reopens the dialog for the next text, so it scrolls back to the top. -->
  {#key received}
    <Modal title={$_('text.received_title')} closeLabel={$_('text.close')} onclose={closeReceivedText}>
      <p class="from">
        {$_('text.from', { values: { device: received.deviceName } })}
        <span class="fingerprint">{$_('devices.fingerprint')} <bdi class="ltr">{received.fingerprint}</bdi></span>
      </p>
      <!-- On one line: with `pre-wrap`, spaces around the text would show. -->
      <div class="text" dir="auto" tabindex="0" role="textbox" aria-readonly="true" aria-label={$_('text.label')}>
        {received.text}
      </div>
      {#if waiting > 0}
        <p class="more">{$_('text.more', { values: { count: waiting } })}</p>
      {/if}
      {#snippet actions()}
        <Button onclick={closeReceivedText}>{$_('text.close')}</Button>
        <Button variant="primary" onclick={() => void copy(received.text)}>
          <Icon name="clipboard" size={16} />
          {$_('text.copy')}
        </Button>
      {/snippet}
    </Modal>
  {/key}
{/if}

<style>
  .from {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-2);
    margin: 0;
  }

  .fingerprint {
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }

  .text {
    max-block-size: 50vh;
    padding: var(--space-3);
    overflow: auto;
    border-radius: var(--radius-md);
    background: var(--color-surface-hover);
    font-family: ui-monospace, monospace;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
    user-select: text;
  }

  .text:focus-visible {
    outline: 2px solid var(--color-focus);
    outline-offset: 1px;
  }

  .more {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }
</style>

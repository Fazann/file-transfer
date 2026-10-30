<script lang="ts">
  import { _, locale } from 'svelte-i18n';

  import Badge from '$lib/components/ui/Badge.svelte';
  import Button from '$lib/components/ui/Button.svelte';
  import Modal from '$lib/components/ui/Modal.svelte';
  import { answerOffer, offers } from '$lib/stores/offers';
  import { formatBytes } from '$lib/utils/format';

  import type { Locale } from '$lib/types';

  /** One offer at a time, oldest first. */
  const offer = $derived($offers[0]);
</script>

{#if offer}
  {#key offer.requestId}
    <Modal
      title={$_('offer.title')}
      closeLabel={$_('offer.decline')}
      onclose={() => void answerOffer(offer.requestId, false)}
    >
      <p class="from">{$_('offer.from', { values: { device: offer.deviceName } })}</p>
      <div class="file">
        <bdi class="name">{offer.fileName}</bdi>
        <span class="size">
          {#if offer.fileCount > 1}
            <span>{$_('transfers.files', { values: { count: offer.fileCount } })}</span>
          {/if}
          <span>{formatBytes(offer.size, ($locale ?? 'en') as Locale)}</span>
        </span>
      </div>
      <div class="trust">
        {#if offer.trusted}
          <Badge text={$_('devices.paired')} tone="success" />
        {:else}
          <Badge text={$_('devices.not_paired')} tone="warning" />
          <p class="warning">{$_('offer.not_paired')}</p>
        {/if}
        <span class="fingerprint">{$_('devices.fingerprint')} <bdi class="ltr">{offer.fingerprint}</bdi></span>
      </div>
      {#snippet actions()}
        <Button onclick={() => void answerOffer(offer.requestId, false)}>{$_('offer.decline')}</Button>
        <Button variant="primary" onclick={() => void answerOffer(offer.requestId, true)}>{$_('offer.accept')}</Button>
      {/snippet}
    </Modal>
  {/key}
{/if}

<style>
  .from {
    margin: 0;
  }

  .file {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3);
    border-radius: var(--radius-md);
    background: var(--color-surface-hover);
  }

  .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .size {
    flex-shrink: 0;
    color: var(--color-text-muted);
  }

  .size > span + span::before {
    content: ' · ';
  }

  .trust {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }

  .warning {
    margin: 0;
    color: var(--color-warning);
  }

  .fingerprint {
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }
</style>

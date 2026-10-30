<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { _, locale } from 'svelte-i18n';

  import { onFileDrop } from '$lib/api';
  import EmptyState from '$lib/components/ui/EmptyState.svelte';
  import { sendToPeer } from '$lib/stores/pairing';
  import { peers } from '$lib/stores/peers';
  import { showToast } from '$lib/stores/toasts';
  import { isCommandError } from '$lib/types';
  import { errorMessage } from '$lib/utils/errors';

  import DeviceCard from './DeviceCard.svelte';

  import type { Locale, Peer } from '$lib/types';

  let hovered: string | null = $state(null);
  let stopListening: (() => void) | null = null;

  /** The device card under a point of the window, if any. */
  function peerAt(x: number, y: number): Peer | undefined {
    const card = document.elementFromPoint(x, y)?.closest<HTMLElement>('[data-peer-id]');
    return $peers.find((peer) => peer.id === card?.dataset.peerId);
  }

  function reportError(error: unknown, peer: Peer): void {
    if (isCommandError(error)) {
      showToast(errorMessage($_, error, ($locale ?? 'en') as Locale, { device: peer.name }), 'error');
    } else {
      console.error(error);
    }
  }

  onMount(async () => {
    stopListening = await onFileDrop((drop) => {
      if (drop.kind === 'leave') {
        hovered = null;
        return;
      }
      const peer = peerAt(drop.x, drop.y);
      hovered = peer?.id ?? null;
      if (drop.kind === 'drop') {
        hovered = null;
        if (peer !== undefined) {
          sendToPeer(peer, drop.paths).catch((error: unknown) => reportError(error, peer));
        }
      }
    });
  });

  onDestroy(() => stopListening?.());
</script>

{#if $peers.length === 0}
  <EmptyState icon="devices" title={$_('devices.empty_title')} description={$_('devices.empty_description')} />
{:else}
  <p class="hint">{$_('devices.drop_hint')}</p>
  <div class="grid">
    {#each $peers as peer (peer.id)}
      <DeviceCard {peer} dropTarget={hovered === peer.id} onerror={reportError} />
    {/each}
  </div>
{/if}

<style>
  .hint {
    margin: 0;
    color: var(--color-text-muted);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(15rem, 1fr));
    gap: var(--space-4);
  }
</style>

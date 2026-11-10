<script lang="ts">
  import { _ } from 'svelte-i18n';

  import { pickFiles, pickFolder } from '$lib/api';
  import Badge from '$lib/components/ui/Badge.svelte';
  import Button from '$lib/components/ui/Button.svelte';
  import Card from '$lib/components/ui/Card.svelte';
  import Icon from '$lib/components/ui/Icon.svelte';
  import { pairWith, sendToPeer } from '$lib/stores/pairing';
  import { openSendText } from '$lib/stores/texts';

  import type { Peer } from '$lib/types';

  interface Props {
    peer: Peer;
    /** Files are being dragged over this card. */
    dropTarget: boolean;
    onerror: (error: unknown, peer: Peer) => void;
  }

  let { peer, dropTarget, onerror }: Props = $props();

  async function chooseAndSend(): Promise<void> {
    try {
      await sendToPeer(peer, await pickFiles($_('devices.pick_title')));
    } catch (error) {
      onerror(error, peer);
    }
  }

  async function chooseFolderAndSend(): Promise<void> {
    try {
      const folder = await pickFolder($_('devices.pick_folder_title'));
      if (folder !== null) {
        await sendToPeer(peer, [folder]);
      }
    } catch (error) {
      onerror(error, peer);
    }
  }

  async function pair(): Promise<void> {
    try {
      await pairWith(peer);
    } catch (error) {
      onerror(error, peer);
    }
  }
</script>

<!-- `data-peer-id` lets the drop handler find the card under the cursor. -->
<div class="device" data-peer-id={peer.id}>
  <Card highlighted={dropTarget}>
    <div class="head">
      <Icon name={peer.os} size={32} />
      <div class="text">
        <strong class="name" title={peer.name}>{peer.name}</strong>
        <span class="meta"><span>{$_(`devices.os.${peer.os}`)}</span><bdi class="ltr">{peer.fingerprint}</bdi></span>
      </div>
    </div>
    <div class="status">
      {#if dropTarget}
        <Badge text={$_('devices.drop_here')} tone="accent" />
      {:else if peer.trusted}
        <Badge text={$_('devices.paired')} tone="success" />
      {:else}
        <Badge text={$_('devices.not_paired')} tone="warning" />
      {/if}
    </div>
    <div class="actions">
      <Button variant="primary" onclick={chooseAndSend}>
        <Icon name="send" size={16} />
        {$_('devices.send_files')}
      </Button>
      <Button onclick={chooseFolderAndSend}>{$_('devices.send_folder')}</Button>
      <!-- Texts go to paired devices only, so an unpaired one offers pairing instead. -->
      {#if peer.trusted}
        <Button onclick={() => openSendText(peer)}>{$_('devices.send_text')}</Button>
      {:else}
        <Button onclick={pair}>{$_('devices.pair')}</Button>
      {/if}
    </div>
  </Card>
</div>

<style>
  .device {
    min-inline-size: 0;
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    color: var(--color-text-muted);
  }

  .text {
    display: flex;
    flex-direction: column;
    min-inline-size: 0;
  }

  .name {
    color: var(--color-text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meta {
    font-size: var(--font-size-sm);
  }

  .meta > * + *::before {
    content: ' · ';
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
</style>

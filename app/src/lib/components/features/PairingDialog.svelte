<script lang="ts">
  import { _ } from 'svelte-i18n';

  import Button from '$lib/components/ui/Button.svelte';
  import Modal from '$lib/components/ui/Modal.svelte';
  import PinInput from '$lib/components/ui/PinInput.svelte';
  import { closePairing, enterPin, pairing } from '$lib/stores/pairing';

  let pin = $state('');

  $effect(() => {
    // A new pairing starts with empty boxes.
    if ($pairing === null) {
      pin = '';
    }
  });
</script>

{#if $pairing}
  <Modal title={$_('pairing.title')} closeLabel={$_('common.cancel')} onclose={() => void closePairing()}>
    {#if $pairing.kind === 'show'}
      <p>{$_('pairing.show_pin', { values: { device: $pairing.deviceName } })}</p>
      <p class="pin" dir="ltr">{$pairing.pin.slice(0, 3)} {$pairing.pin.slice(3)}</p>
      <p class="fingerprint">{$_('devices.fingerprint')} <bdi class="ltr">{$pairing.fingerprint}</bdi></p>
    {:else if $pairing.submitted}
      <p>{$_('pairing.waiting', { values: { device: $pairing.peer.name } })}</p>
    {:else}
      <p>{$_('pairing.enter_pin', { values: { device: $pairing.peer.name } })}</p>
      <PinInput bind:value={pin} label={$_('pairing.pin_label')} oncomplete={(value) => void enterPin(value)} />
      <p class="fingerprint">{$_('devices.fingerprint')} <bdi class="ltr">{$pairing.peer.fingerprint}</bdi></p>
    {/if}
    {#snippet actions()}
      <Button onclick={() => void closePairing()}>{$_('common.cancel')}</Button>
      {#if $pairing?.kind === 'enter' && !$pairing.submitted}
        <Button variant="primary" disabled={pin.length !== 6} onclick={() => void enterPin(pin)}>
          {$_('pairing.pair')}
        </Button>
      {/if}
    {/snippet}
  </Modal>
{/if}

<style>
  p {
    margin: 0;
  }

  .pin {
    font-size: 2.5rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.15em;
    text-align: center;
  }

  .fingerprint {
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }
</style>

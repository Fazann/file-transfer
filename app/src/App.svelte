<script lang="ts">
  import { onMount } from 'svelte';
  import { _, locale as i18nLocale } from 'svelte-i18n';

  import { notify, onEvent, setTrayLabels } from '$lib/api';
  import IncomingOfferDialog from '$lib/components/features/IncomingOfferDialog.svelte';
  import PairingDialog from '$lib/components/features/PairingDialog.svelte';
  import ReceivedTextDialog from '$lib/components/features/ReceivedTextDialog.svelte';
  import SendTextDialog from '$lib/components/features/SendTextDialog.svelte';
  import ToastHost from '$lib/components/features/ToastHost.svelte';
  import UpdateDialog from '$lib/components/features/UpdateDialog.svelte';
  import Sidebar from '$lib/components/ui/Sidebar.svelte';
  import SidebarItem from '$lib/components/ui/SidebarItem.svelte';
  import { systemLocale } from '$lib/i18n';
  import { startHistory } from '$lib/stores/history';
  import { setLocale } from '$lib/stores/locale';
  import { startOffers } from '$lib/stores/offers';
  import { startPairingEvents, type PairingOutcome } from '$lib/stores/pairing';
  import { startPeers } from '$lib/stores/peers';
  import { loadSettings } from '$lib/stores/settings';
  import { startTexts } from '$lib/stores/texts';
  import { applyTheme } from '$lib/stores/theme';
  import { showToast } from '$lib/stores/toasts';
  import { activeCount, startTransfers } from '$lib/stores/transfers';
  import { startUpdateCheck } from '$lib/stores/update';
  import { errorMessage } from '$lib/utils/errors';
  import { formatBytes } from '$lib/utils/format';

  import DevicesPage from './pages/DevicesPage.svelte';
  import HistoryPage from './pages/HistoryPage.svelte';
  import SettingsPage from './pages/SettingsPage.svelte';
  import TransfersPage from './pages/TransfersPage.svelte';

  import type { IconName } from '$lib/components/ui/Icon.svelte';
  import type { CommandError, IncomingOffer, Locale, ReceivedText } from '$lib/types';

  type Page = 'devices' | 'transfers' | 'history' | 'settings';

  const PAGES: { id: Page; icon: IconName }[] = [
    { id: 'devices', icon: 'devices' },
    { id: 'transfers', icon: 'transfers' },
    { id: 'history', icon: 'history' },
    { id: 'settings', icon: 'settings' },
  ];

  let page: Page = $state('devices');
  let collapsed = $state(false);
  const current = $derived(($i18nLocale ?? 'en') as Locale);

  function offerNotification(offer: IncomingOffer): { title: string; body: string } {
    const values = { device: offer.deviceName, file: offer.fileName, size: formatBytes(offer.size, current) };
    return { title: $_('notifications.offer_title'), body: $_('notifications.offer_body', { values }) };
  }

  /** Names the sender only: the text itself may be a password, and notifications show on the lock screen. */
  function textNotification(text: ReceivedText): { title: string; body: string } {
    const values = { device: text.deviceName };
    return { title: $_('notifications.text_title'), body: $_('notifications.text_body', { values }) };
  }

  /** Only the user here can fix what refused the offer, so they hear about it even with the window hidden. */
  function receiveRefused(error: CommandError): void {
    const message = errorMessage($_, error, current);
    showToast(message, 'error');
    void notify($_('notifications.receive_refused_title'), message);
  }

  function pairingDone(outcome: PairingOutcome): void {
    if (outcome.error === null) {
      showToast($_('pairing.success', { values: { device: outcome.deviceName } }), 'success');
    } else {
      showToast(errorMessage($_, outcome.error, current, { device: outcome.deviceName }), 'error');
    }
  }

  // Text outside the window (the tray menu) is translated here and handed to Rust.
  $effect(() => {
    void setTrayLabels({ open: $_('tray.open'), pauseAll: $_('tray.pause_all'), quit: $_('tray.quit') });
  });

  onMount(async () => {
    const settings = await loadSettings();
    setLocale(settings.language ?? systemLocale());
    applyTheme(settings.theme);
    await Promise.all([
      startPeers(),
      startTransfers(),
      startOffers(offerNotification),
      startTexts(textNotification),
      startPairingEvents(pairingDone),
      startHistory(),
      onEvent('receive-refused', receiveRefused),
      onEvent('clipboard-received', ({ deviceName }) =>
        showToast($_('text.clipboard_received', { values: { device: deviceName } })),
      ),
    ]);
    void startUpdateCheck();
  });
</script>

<div class="shell">
  <Sidebar bind:collapsed collapseLabel={$_('nav.collapse')} expandLabel={$_('nav.expand')}>
    {#snippet header()}
      <strong class="brand">{$_('app.name')}</strong>
    {/snippet}
    {#each PAGES as item (item.id)}
      <SidebarItem
        icon={item.icon}
        label={$_(`nav.${item.id}`)}
        active={page === item.id}
        {collapsed}
        count={item.id === 'transfers' ? $activeCount : 0}
        onclick={() => (page = item.id)}
      />
    {/each}
  </Sidebar>
  <main>
    {#if page === 'devices'}
      <DevicesPage />
    {:else if page === 'transfers'}
      <TransfersPage />
    {:else if page === 'history'}
      <HistoryPage />
    {:else}
      <SettingsPage />
    {/if}
  </main>
</div>

<IncomingOfferDialog />
<ReceivedTextDialog />
<SendTextDialog />
<PairingDialog />
<UpdateDialog />
<ToastHost />

<style>
  .shell {
    display: flex;
    block-size: 100%;
  }

  main {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: var(--space-4);
    min-inline-size: 0;
    padding: var(--space-6);
    overflow-y: auto;
  }

  main :global(h1) {
    margin: 0;
    font-size: var(--font-size-xl);
  }

  .brand {
    font-size: var(--font-size-lg);
  }
</style>

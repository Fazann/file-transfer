<script lang="ts">
  import { onMount } from 'svelte';
  import { _, locale } from 'svelte-i18n';

  import { listTrusted, removeTrusted, setAutoAccept, setSyncClipboard } from '$lib/api';
  import Button from '$lib/components/ui/Button.svelte';
  import Checkbox from '$lib/components/ui/Checkbox.svelte';
  import { markTrusted } from '$lib/stores/peers';
  import { formatDate } from '$lib/utils/format';

  import type { Locale, TrustedDevice } from '$lib/types';

  let devices: TrustedDevice[] = $state([]);

  async function reload(): Promise<void> {
    devices = await listTrusted();
  }

  async function remove(device: TrustedDevice): Promise<void> {
    await removeTrusted(device.id);
    markTrusted(device.id, false);
    await reload();
  }

  onMount(() => void reload());
</script>

<section>
  <h2>{$_('settings.trusted_devices')}</h2>
  {#if devices.length === 0}
    <p class="empty">{$_('settings.trusted_empty')}</p>
  {:else}
    <ul>
      {#each devices as device (device.id)}
        <li>
          <div class="text">
            <strong class="name">{device.name}</strong>
            <span class="meta">
              <bdi class="ltr">{device.fingerprint}</bdi>
              <span>
                {$_('settings.paired_on', {
                  values: { date: formatDate(device.pairedAt, ($locale ?? 'en') as Locale) },
                })}
              </span>
            </span>
            <Checkbox
              label={$_('settings.auto_accept')}
              checked={device.autoAccept}
              onchange={(checked) => void setAutoAccept(device.id, checked)}
            />
            <Checkbox
              label={$_('settings.sync_clipboard')}
              description={$_('settings.sync_clipboard_hint')}
              checked={device.syncClipboard}
              onchange={(checked) => void setSyncClipboard(device.id, checked)}
            />
          </div>
          <Button variant="ghost" onclick={() => void remove(device)}>{$_('settings.remove')}</Button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  section {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  h2 {
    margin: 0;
    font-size: var(--font-size-lg);
  }

  .empty {
    margin: 0;
    color: var(--color-text-muted);
  }

  ul {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-lg);
    background: var(--color-surface);
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    min-inline-size: 0;
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meta {
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }

  .meta > * + *::before {
    content: ' · ';
  }
</style>

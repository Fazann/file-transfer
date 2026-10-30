<script lang="ts">
  import { onMount } from 'svelte';
  import { _, locale } from 'svelte-i18n';

  import { openFolder } from '$lib/api';
  import Badge from '$lib/components/ui/Badge.svelte';
  import Button from '$lib/components/ui/Button.svelte';
  import EmptyState from '$lib/components/ui/EmptyState.svelte';
  import Icon from '$lib/components/ui/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import { clearHistory, history, loadHistory } from '$lib/stores/history';
  import { formatBytes, formatDate } from '$lib/utils/format';

  import type { HistoryStatus, Locale } from '$lib/types';

  const current = $derived(($locale ?? 'en') as Locale);
  const tones: Record<HistoryStatus, 'success' | 'warning' | 'neutral'> = {
    completed: 'success',
    failed: 'warning',
    cancelled: 'neutral',
  };

  onMount(() => void loadHistory());
</script>

{#if $history.length === 0}
  <EmptyState icon="history" title={$_('history.empty_title')} description={$_('history.empty_description')} />
{:else}
  <div class="toolbar">
    <Button variant="ghost" onclick={() => void clearHistory()}>{$_('history.clear')}</Button>
  </div>
  <ul class="list">
    {#each $history as entry (entry.id)}
      <li class="row">
        <Icon name={entry.direction === 'send' ? 'send' : 'receive'} />
        <div class="main">
          <bdi class="file" title={entry.fileName}>{entry.fileName}</bdi>
          <span class="meta">
            <span>
              {$_(entry.direction === 'send' ? 'history.sent_to' : 'history.received_from', {
                values: { device: entry.peerName },
              })}
            </span>
            <span>{formatBytes(entry.size, current)}</span>
            <span>{formatDate(entry.finishedAt, current)}</span>
          </span>
        </div>
        <Badge text={$_(`history.status.${entry.status}`)} tone={tones[entry.status]} />
        {#if entry.path !== null}
          <IconButton icon="folder" label={$_('history.open_folder')} onclick={() => void openFolder(entry.path)} />
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .toolbar {
    display: flex;
    justify-content: flex-end;
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding-block: var(--space-2);
    padding-inline: var(--space-3);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-lg);
    background: var(--color-surface);
    color: var(--color-text-muted);
  }

  .main {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-inline-size: 0;
  }

  .file {
    color: var(--color-text);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meta span + span::before {
    content: ' · ';
  }

  .meta {
    font-size: var(--font-size-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>

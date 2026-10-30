<script lang="ts">
  import { _, locale } from 'svelte-i18n';

  import { openFolder } from '$lib/api';
  import Icon from '$lib/components/ui/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import ProgressBar from '$lib/components/ui/ProgressBar.svelte';
  import { cancel, pause, resume } from '$lib/stores/transfers';
  import { errorMessage } from '$lib/utils/errors';
  import { formatBytes, formatDuration, formatSpeed } from '$lib/utils/format';

  import type { Locale, Transfer } from '$lib/types';

  interface Props {
    transfer: Transfer;
  }

  let { transfer }: Props = $props();

  const current = $derived(($locale ?? 'en') as Locale);
  const fraction = $derived(
    transfer.size === 0 ? (transfer.status === 'completed' ? 1 : 0) : transfer.transferred / transfer.size,
  );
  const active = $derived(transfer.status === 'active');
  const waiting = $derived(active && transfer.transferred === 0 && transfer.speed === 0 && !transfer.paused);
  const tone = $derived(
    transfer.status === 'completed'
      ? 'success'
      : transfer.status === 'active'
        ? transfer.paused
          ? 'paused'
          : 'accent'
        : 'danger',
  );
  const peerLine = $derived(
    $_(transfer.direction === 'send' ? 'transfers.sending_to' : 'transfers.receiving_from', {
      values: { device: transfer.peerName },
    }),
  );

  /** Each part is shown in its own span; CSS puts a separator between them. */
  const statusParts = $derived.by((): string[] => {
    if (transfer.status === 'failed' && transfer.error !== null) {
      return [errorMessage($_, transfer.error, current, { device: transfer.peerName })];
    }
    if (active && transfer.paused) {
      return [$_('transfers.status.paused')];
    }
    if (waiting) {
      return [$_('transfers.status.waiting')];
    }
    if (active && transfer.speed > 0) {
      const left = (transfer.size - transfer.transferred) / transfer.speed;
      const time = formatDuration(left, current);
      return [formatSpeed(transfer.speed, current), $_('transfers.time_left', { values: { time } })];
    }
    return [$_(`transfers.status.${transfer.status}`)];
  });
</script>

<li class="row">
  <Icon name={transfer.direction === 'send' ? 'send' : 'receive'} />
  <div class="main">
    <div class="line">
      <bdi class="file" title={transfer.fileName}>{transfer.fileName}</bdi>
      <span class="peer">{peerLine}</span>
    </div>
    {#if transfer.fileCount > 1}
      <div class="line meta">
        <bdi class="current">{transfer.currentFile ?? ''}</bdi>
        <span>{$_('transfers.files', { values: { count: transfer.fileCount } })}</span>
      </div>
    {/if}
    <ProgressBar value={fraction} label={transfer.fileName} {tone} />
    <div class="line meta">
      <span>
        {$_('transfers.progress', {
          values: { done: formatBytes(transfer.transferred, current), total: formatBytes(transfer.size, current) },
        })}
      </span>
      <span class="status parts" class:error={transfer.status === 'failed'}>
        {#each statusParts as part, index (index)}
          <span>{part}</span>
        {/each}
      </span>
    </div>
  </div>
  <div class="actions">
    {#if active}
      {#if transfer.paused}
        <IconButton icon="play" label={$_('transfers.resume')} onclick={() => void resume(transfer.id)} />
      {:else}
        <IconButton icon="pause" label={$_('transfers.pause')} onclick={() => void pause(transfer.id)} />
      {/if}
      <IconButton icon="close" label={$_('transfers.cancel')} onclick={() => void cancel(transfer.id)} />
    {:else if transfer.path !== null}
      <IconButton icon="folder" label={$_('transfers.open_folder')} onclick={() => void openFolder(transfer.path)} />
    {/if}
  </div>
</li>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-lg);
    background: var(--color-surface);
    color: var(--color-text-muted);
  }

  .main {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: var(--space-1);
    min-inline-size: 0;
  }

  .line {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    min-inline-size: 0;
  }

  .file {
    color: var(--color-text);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .peer {
    flex-shrink: 0;
    max-inline-size: 45%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meta {
    font-size: var(--font-size-sm);
  }

  .current {
    min-inline-size: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .status {
    text-align: end;
  }

  .parts span + span::before {
    content: ' · ';
  }

  .error {
    color: var(--color-danger);
  }

  .actions {
    display: flex;
  }
</style>

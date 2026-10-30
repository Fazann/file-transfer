<script lang="ts">
  import { _ } from 'svelte-i18n';

  import Button from '$lib/components/ui/Button.svelte';
  import EmptyState from '$lib/components/ui/EmptyState.svelte';
  import { clearFinished, transfers } from '$lib/stores/transfers';

  import TransferRow from './TransferRow.svelte';

  /** Newest first. */
  const list = $derived([...$transfers.values()].reverse());
  const hasFinished = $derived(list.some((transfer) => transfer.status !== 'active'));
</script>

{#if list.length === 0}
  <EmptyState icon="transfers" title={$_('transfers.empty_title')} description={$_('transfers.empty_description')} />
{:else}
  {#if hasFinished}
    <div class="toolbar">
      <Button variant="ghost" onclick={clearFinished}>{$_('transfers.clear_finished')}</Button>
    </div>
  {/if}
  <ul class="list">
    {#each list as transfer (transfer.id)}
      <TransferRow {transfer} />
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
</style>

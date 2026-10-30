<script lang="ts">
  import { _ } from 'svelte-i18n';

  import Icon from '$lib/components/ui/Icon.svelte';
  import { self } from '$lib/stores/peers';
</script>

{#if $self}
  <section class="this-device">
    <Icon name="shield" size={28} />
    <div class="text">
      <span class="label">{$_('devices.this_device')}</span>
      <strong class="name">{$self.name}</strong>
      <span class="fingerprint">
        {$_('devices.fingerprint')}
        <bdi class="ltr">{$self.fingerprint}</bdi>
      </span>
    </div>
  </section>
{/if}

<style>
  .this-device {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    background: var(--color-accent-soft);
    color: var(--color-accent-strong);
  }

  .text {
    display: flex;
    flex-direction: column;
    min-inline-size: 0;
  }

  .label,
  .fingerprint {
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }

  .name {
    color: var(--color-text);
    font-size: var(--font-size-lg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>

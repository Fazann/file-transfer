<script lang="ts">
  import Icon from './Icon.svelte';
  import IconButton from './IconButton.svelte';

  interface Props {
    message: string;
    tone?: 'info' | 'success' | 'error';
    dismissLabel: string;
    ondismiss: () => void;
  }

  let { message, tone = 'info', dismissLabel, ondismiss }: Props = $props();
</script>

<div class="toast {tone}" role={tone === 'error' ? 'alert' : 'status'}>
  <Icon name={tone === 'error' ? 'alert' : 'check'} />
  <p>{message}</p>
  <IconButton icon="close" label={dismissLabel} onclick={ondismiss} />
</div>

<style>
  .toast {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding-block: var(--space-2);
    padding-inline: var(--space-3);
    border: 1px solid var(--color-border);
    border-inline-start-width: 4px;
    border-radius: var(--radius-md);
    background: var(--color-surface);
    box-shadow: var(--shadow-lg);
  }

  p {
    flex: 1;
    margin: 0;
  }

  .info {
    border-inline-start-color: var(--color-accent);
  }

  .success {
    border-inline-start-color: var(--color-success);
  }

  .error {
    border-inline-start-color: var(--color-danger);
  }
</style>

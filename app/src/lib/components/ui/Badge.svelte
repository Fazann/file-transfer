<script lang="ts">
  interface Props {
    text: string;
    tone?: 'neutral' | 'success' | 'warning' | 'accent';
  }

  let { text, tone = 'neutral' }: Props = $props();
</script>

<span class="badge {tone}">{text}</span>

<style>
  .badge {
    display: inline-block;
    max-inline-size: 100%;
    padding-block: 0.1rem;
    padding-inline: var(--space-2);
    border-radius: 999px;
    font-size: var(--font-size-sm);
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .neutral {
    background: var(--color-surface-hover);
    color: var(--color-text-muted);
  }

  .success {
    background: var(--color-success-soft);
    color: var(--color-success);
  }

  .warning {
    background: var(--color-warning-soft);
    color: var(--color-warning);
  }

  .accent {
    background: var(--color-accent);
    color: var(--color-on-accent);
  }
</style>

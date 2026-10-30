<script lang="ts">
  interface Props {
    /** From 0 to 1. */
    value: number;
    /** Read by screen readers. */
    label: string;
    tone?: 'accent' | 'paused' | 'success' | 'danger';
  }

  let { value, label, tone = 'accent' }: Props = $props();

  const percent = $derived(Math.round(Math.min(1, Math.max(0, value)) * 100));
</script>

<div class="track" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}>
  <div class="fill {tone}" style:inline-size="{percent}%"></div>
</div>

<style>
  /* The fill grows from the start edge: left in LTR, right in RTL. */
  .track {
    display: flex;
    block-size: 0.5rem;
    border-radius: 999px;
    background: var(--color-surface-hover);
    overflow: hidden;
  }

  .fill {
    border-radius: 999px;
    transition: inline-size 200ms linear;
  }

  .accent {
    background: var(--color-accent);
  }

  .paused {
    background: var(--color-text-muted);
  }

  .success {
    background: var(--color-success);
  }

  .danger {
    background: var(--color-danger);
  }
</style>

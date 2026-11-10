<script lang="ts">
  import { _, locale } from 'svelte-i18n';

  import { readClipboard } from '$lib/api';
  import Button from '$lib/components/ui/Button.svelte';
  import Icon from '$lib/components/ui/Icon.svelte';
  import Modal from '$lib/components/ui/Modal.svelte';
  import { closeSendText, sendTextTo, textTarget, utf8Length } from '$lib/stores/texts';
  import { showToast } from '$lib/stores/toasts';
  import { isCommandError, MAX_TEXT_BYTES } from '$lib/types';
  import { errorMessage } from '$lib/utils/errors';
  import { formatBytes } from '$lib/utils/format';

  import type { Locale, Peer } from '$lib/types';

  let text = $state('');
  let sending = $state(false);
  let field: HTMLTextAreaElement | undefined = $state();

  const current = $derived(($locale ?? 'en') as Locale);
  const bytes = $derived(utf8Length(text));
  const tooLong = $derived(bytes > MAX_TEXT_BYTES);

  // Every time the dialog opens, it starts empty with the cursor in the text box.
  $effect(() => {
    if ($textTarget !== null) {
      text = '';
      sending = false;
      field?.focus();
    }
  });

  /** Puts the clipboard's text where the cursor is, replacing any selection. */
  async function paste(): Promise<void> {
    const clipboard = await readClipboard();
    if (clipboard === '') {
      showToast($_('text.clipboard_empty'));
      return;
    }
    if (field === undefined) {
      text += clipboard;
      return;
    }
    field.setRangeText(clipboard, field.selectionStart, field.selectionEnd, 'end');
    text = field.value;
    field.focus();
  }

  async function send(peer: Peer): Promise<void> {
    sending = true;
    try {
      await sendTextTo(peer, text);
      showToast($_('text.sent', { values: { device: peer.name } }), 'success');
      // The user may have closed the dialog, or opened it for another device, while this was on its way.
      if ($textTarget?.id === peer.id) {
        closeSendText();
      }
    } catch (error) {
      if (isCommandError(error)) {
        showToast(errorMessage($_, error, current, { device: peer.name }), 'error');
      } else {
        console.error(error);
      }
    } finally {
      sending = false;
    }
  }
</script>

{#if $textTarget}
  {@const peer = $textTarget}
  <Modal
    title={$_('text.send_title', { values: { device: peer.name } })}
    closeLabel={$_('text.cancel')}
    onclose={closeSendText}
  >
    <label class="label" for="send-text">{$_('text.label')}</label>
    <textarea
      id="send-text"
      bind:this={field}
      bind:value={text}
      dir="auto"
      rows="6"
      spellcheck="false"
      aria-describedby="send-text-size"
      onkeydown={(event) => {
        // Ctrl+Enter or Cmd+Enter sends, as in most chat apps; Enter alone starts a new line.
        if (event.key === 'Enter' && (event.ctrlKey || event.metaKey) && text !== '' && !tooLong && !sending) {
          event.preventDefault();
          void send(peer);
        }
      }}></textarea>
    <div class="row">
      <Button variant="ghost" onclick={() => void paste()}>
        <Icon name="clipboard" size={16} />
        {$_('text.paste')}
      </Button>
      <span id="send-text-size" class="size" class:over={tooLong}>
        {$_('text.size', {
          values: { used: formatBytes(bytes, current), max: formatBytes(MAX_TEXT_BYTES, current) },
        })}
      </span>
    </div>
    {#if tooLong}
      <p class="over">{$_('text.too_long')}</p>
    {/if}
    {#snippet actions()}
      <Button onclick={closeSendText}>{$_('text.cancel')}</Button>
      <Button variant="primary" disabled={text === '' || tooLong || sending} onclick={() => void send(peer)}>
        <Icon name="send" size={16} />
        {sending ? $_('text.sending') : $_('text.send')}
      </Button>
    {/snippet}
  </Modal>
{/if}

<style>
  .label {
    font-weight: 600;
  }

  textarea {
    min-block-size: 8rem;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-background);
    color: var(--color-text);
    font: inherit;
    resize: vertical;
  }

  textarea:focus-visible {
    outline: 2px solid var(--color-focus);
    outline-offset: 1px;
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .size {
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }

  .over {
    margin: 0;
    color: var(--color-danger);
  }
</style>

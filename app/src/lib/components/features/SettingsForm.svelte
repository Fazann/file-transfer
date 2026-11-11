<script lang="ts">
  import { _, locale as i18nLocale } from 'svelte-i18n';

  import { exportDiagnostics, pickFolder, pickSavePath } from '$lib/api';
  import Button from '$lib/components/ui/Button.svelte';
  import Checkbox from '$lib/components/ui/Checkbox.svelte';
  import Input from '$lib/components/ui/Input.svelte';
  import Select from '$lib/components/ui/Select.svelte';
  import { isLocale, LOCALES, systemLocale } from '$lib/i18n';
  import { setLocale } from '$lib/stores/locale';
  import { refreshSelf } from '$lib/stores/peers';
  import { saveSettings, settings } from '$lib/stores/settings';
  import { applyTheme } from '$lib/stores/theme';
  import { showToast } from '$lib/stores/toasts';
  import { isCommandError, isTheme, THEMES } from '$lib/types';
  import { errorMessage } from '$lib/utils/errors';

  import type { Locale, Settings } from '$lib/types';

  /** `system` follows the operating system's language or appearance. */
  const SYSTEM = 'system';

  let draft: Settings | null = $state(null);
  let language = $state(SYSTEM);
  let theme = $state(SYSTEM);
  let saving = $state(false);

  $effect(() => {
    if (draft === null && $settings !== null) {
      draft = { ...$settings };
      language = $settings.language ?? SYSTEM;
      theme = $settings.theme ?? SYSTEM;
    }
  });

  const languageOptions = $derived([
    { value: SYSTEM, label: $_('settings.language_system') },
    ...LOCALES.map((code) => ({ value: code, label: $_(`languages.${code}`) })),
  ]);

  const themeOptions = $derived([
    { value: SYSTEM, label: $_('settings.theme_system') },
    ...THEMES.map((code) => ({ value: code, label: $_(`settings.theme_${code}`) })),
  ]);

  /** Language applies at once and is saved with the other settings. */
  function changeLanguage(value: string): void {
    setLocale(isLocale(value) ? value : systemLocale());
  }

  /** Like language, the theme applies at once and is saved with the other settings. */
  function changeTheme(value: string): void {
    applyTheme(isTheme(value) ? value : null);
  }

  async function chooseFolder(): Promise<void> {
    const folder = await pickFolder($_('settings.choose_folder_title'));
    if (folder !== null && draft !== null) {
      draft.downloadDir = folder;
    }
  }

  function showError(error: unknown): void {
    const message = isCommandError(error) ? errorMessage($_, error, ($i18nLocale ?? 'en') as Locale) : String(error);
    showToast(message, 'error');
  }

  async function exportLogs(): Promise<void> {
    const destination = await pickSavePath($_('settings.export_diagnostics_title'), 'lanshare-diagnostics.zip');
    if (destination === null) {
      return;
    }
    try {
      await exportDiagnostics(destination);
      showToast($_('settings.diagnostics_saved'), 'success');
    } catch (error) {
      showError(error);
    }
  }

  async function save(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (draft === null) {
      return;
    }
    saving = true;
    try {
      const next: Settings = {
        ...draft,
        port: Number(draft.port),
        streams: Number(draft.streams),
        chunkSizeMib: Number(draft.chunkSizeMib),
        bandwidthLimitMbps: Number(draft.bandwidthLimitMbps),
        language: isLocale(language) ? language : null,
        theme: isTheme(theme) ? theme : null,
      };
      draft = { ...(await saveSettings(next)) };
      await refreshSelf();
      showToast($_('settings.saved'), 'success');
    } catch (error) {
      showError(error);
    } finally {
      saving = false;
    }
  }
</script>

{#if draft}
  <form class="form" onsubmit={save}>
    <section>
      <h2>{$_('settings.general')}</h2>
      <Select
        id="language"
        label={$_('settings.language')}
        options={languageOptions}
        bind:value={language}
        onchange={changeLanguage}
      />
      <Select
        id="theme"
        label={$_('settings.theme')}
        options={themeOptions}
        bind:value={theme}
        onchange={changeTheme}
      />
      <Checkbox label={$_('settings.close_to_tray')} bind:checked={draft.closeToTray} />
      <Checkbox label={$_('settings.start_at_login')} bind:checked={draft.startAtLogin} />
    </section>

    <section>
      <h2>{$_('settings.network')}</h2>
      <Input id="device-name" label={$_('settings.device_name')} bind:value={draft.deviceName} />
      <div class="folder">
        <Input
          id="download-dir"
          label={$_('settings.download_folder')}
          bind:value={draft.downloadDir}
          dir="ltr"
          readonly
        />
        <Button onclick={() => void chooseFolder()}>{$_('settings.choose_folder')}</Button>
      </div>
      <div class="numbers">
        <Input
          id="port"
          type="number"
          min={1}
          max={65535}
          dir="ltr"
          label={$_('settings.port')}
          bind:value={draft.port}
        />
        <Input
          id="streams"
          type="number"
          min={1}
          max={16}
          dir="ltr"
          label={$_('settings.streams')}
          hint={$_('settings.streams_hint')}
          bind:value={draft.streams}
        />
        <Input
          id="chunk-size"
          type="number"
          min={1}
          max={64}
          dir="ltr"
          label={$_('settings.chunk_size')}
          hint={$_('settings.chunk_size_hint')}
          bind:value={draft.chunkSizeMib}
        />
        <Input
          id="limit"
          type="number"
          min={0}
          dir="ltr"
          label={$_('settings.bandwidth_limit')}
          hint={$_('settings.bandwidth_hint')}
          bind:value={draft.bandwidthLimitMbps}
        />
      </div>
      <Checkbox
        label={$_('settings.allow_untrusted')}
        description={$_('settings.allow_untrusted_hint')}
        bind:checked={draft.allowUntrusted}
      />
      <p class="note">{$_('settings.restart_note')}</p>
    </section>

    <section>
      <h2>{$_('settings.diagnostics')}</h2>
      <p class="note">{$_('settings.export_diagnostics_hint')}</p>
      <div>
        <Button onclick={() => void exportLogs()}>{$_('settings.export_diagnostics')}</Button>
      </div>
    </section>

    <div class="actions">
      <Button variant="primary" type="submit" disabled={saving}>{$_('settings.save')}</Button>
    </div>
  </form>
{/if}

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-6);
  }

  section {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  h2 {
    margin: 0;
    font-size: var(--font-size-lg);
  }

  .folder {
    display: flex;
    align-items: flex-end;
    gap: var(--space-2);
  }

  .folder > :global(:first-child) {
    flex: 1;
  }

  .numbers {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
    gap: var(--space-3);
  }

  .note {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--font-size-sm);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
  }
</style>

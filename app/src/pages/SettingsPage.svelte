<script lang="ts">
  import { _ } from 'svelte-i18n';

  import SettingsForm from '$lib/components/features/SettingsForm.svelte';
  import TrustedDeviceList from '$lib/components/features/TrustedDeviceList.svelte';
</script>

<h1>{$_('settings.title')}</h1>
<SettingsForm />
<TrustedDeviceList />

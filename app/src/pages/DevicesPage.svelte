<script lang="ts">
  import { _ } from 'svelte-i18n';

  import DeviceGrid from '$lib/components/features/DeviceGrid.svelte';
  import ThisDevice from '$lib/components/features/ThisDevice.svelte';
</script>

<h1>{$_('devices.title')}</h1>
<ThisDevice />
<DeviceGrid />

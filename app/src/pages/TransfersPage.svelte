<script lang="ts">
  import { _ } from 'svelte-i18n';

  import TransferList from '$lib/components/features/TransferList.svelte';
</script>

<h1>{$_('transfers.title')}</h1>
<TransferList />

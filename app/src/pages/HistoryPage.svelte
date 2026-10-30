<script lang="ts">
  import { _ } from 'svelte-i18n';

  import HistoryList from '$lib/components/features/HistoryList.svelte';
</script>

<h1>{$_('history.title')}</h1>
<HistoryList />

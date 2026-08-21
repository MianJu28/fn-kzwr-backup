<script>
  import RestoreSection from '../components/RestoreSection.svelte';

  export let restoreFolders = [];
  export let busy = false;
  export let onRestore = null; // (files, sourcePath) => Promise
</script>

<RestoreSection {restoreFolders} {busy} {onRestore} />

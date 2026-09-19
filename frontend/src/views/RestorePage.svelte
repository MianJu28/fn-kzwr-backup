<script>
  import RestoreSection from '../components/RestoreSection.svelte';

  export let restoreFolders = [];
  export let backupPaths = [];
  export let busy = false;
  export let onRestore = null; // (files, sourcePath) => Promise
  export let onGoto = null; // (pageId) => void
</script>

<RestoreSection {restoreFolders} {backupPaths} {busy} {onRestore} {onGoto} />

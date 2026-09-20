<script>
  import RestoreSection from '../components/RestoreSection.svelte';

  export let restoreFolders = [];
  export let backupPaths = [];
  export let busy = false;
  export let onRestore = null; // (files, sourcePath, all) => Promise
  export let onLoadTree = null; // (source, dir) => Promise<{ nodes, error }>
  export let onPrune = null; // (sourcePath) => Promise<{checked, removed, files, error}>
  export let onGoto = null; // (pageId) => void
</script>

<RestoreSection {restoreFolders} {backupPaths} {busy} {onRestore} {onLoadTree} {onPrune} {onGoto} />

<script>
  // 目录树节点（递归）：目录可展开、可整目录恢复；文件可单文件恢复
  import Icon from './components/Icon.svelte';
  import { fmtBytes } from './lib/format.js';

  export let node;
  export let expandedSet = new Set();
  export let busy = false;
  export let onToggleDir = () => {};
  export let onRestore = () => {};
  export let onRestoreDir = () => {};

  const byName = (a, b) => a.name.localeCompare(b.name);

  $: dirs = node.children ? Object.values(node.children).filter((c) => c.is_dir).sort(byName) : [];
  $: files = node.children ? Object.values(node.children).filter((c) => !c.is_dir).sort(byName) : [];

  // 递归收集当前目录下所有文件的相对路径
  function collectDirFiles(n) {
    const out = [];
    const walk = (cur) => {
      if (!cur.children) return;
      for (const child of Object.values(cur.children)) {
        if (child.is_dir) walk(child);
        else out.push(child.rel_path);
      }
    };
    walk(n);
    return out;
  }
</script>

{#if node.is_dir}
  <div class="dir">
    <div class="tree-row">
      <button class="toggle" on:click={() => onToggleDir(node.rel_path)} aria-expanded={expandedSet.has(node.rel_path)}>
        <Icon
          name={expandedSet.has(node.rel_path) ? 'chevron_down' : 'chevron_right'}
          size={14}
        />
      </button>
      <Icon name={expandedSet.has(node.rel_path) ? 'folder-open' : 'folder'} size={14} />
      <span class="name">{node.name}</span>
      <button class="btn btn-sm btn-soft" on:click={() => onRestoreDir(collectDirFiles(node))} disabled={busy}>
        恢复目录
      </button>
    </div>

    {#if expandedSet.has(node.rel_path)}
      <div class="children">
        {#each dirs as child (child.rel_path)}
          <svelte:self
            node={child}
            {expandedSet}
            {busy}
            {onToggleDir}
            {onRestore}
            {onRestoreDir}
          />
        {/each}
        {#each files as f (f.rel_path)}
          <div class="tree-row">
            <span class="indent"></span>
            <Icon name="file" size={14} />
            <span class="name">{f.name}</span>
            <span class="size">{fmtBytes(f.size)}</span>
            <button class="btn btn-sm btn-ghost" on:click={() => onRestore(f.rel_path)} disabled={busy}>
              恢复
            </button>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{:else}
  <div class="tree-row">
    <span class="indent"></span>
    <Icon name="file" size={14} />
    <span class="name">{node.name}</span>
    <span class="size">{fmtBytes(node.size)}</span>
    <button class="btn btn-sm btn-ghost" on:click={() => onRestore(node.rel_path)} disabled={busy}>
      恢复
    </button>
  </div>
{/if}

<style>
  .toggle {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border: none;
    background: transparent;
    color: var(--text-3);
    border-radius: var(--r-xs);
    cursor: pointer;
    flex-shrink: 0;
  }
  .toggle:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .indent {
    width: 20px;
    flex-shrink: 0;
  }
  .children {
    margin-left: 14px;
    padding-left: 8px;
    border-left: 1px dashed var(--border-strong);
  }
  .tree-row .name {
    font-size: 12.5px;
  }
</style>

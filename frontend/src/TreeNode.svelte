<script>
  // 恢复树节点（懒加载：子项由父级按需拉取后经 cache 传入）
  import Icon from './components/Icon.svelte';
  import { fmtBytes } from './lib/format.js';

  export let node;
  /** 展开状态：{ [rel_path]: true } */
  export let expanded = {};
  /** 子项缓存：{ [rel_path]: nodes[] | 'loading' } */
  export let cache = {};
  export let busy = false;
  export let onToggle = () => {}; // (node) => void
  export let onRestoreFile = () => {}; // (node) => void
  export let onRestoreDir = () => {}; // (node) => void

  $: isOpen = !!expanded[node.rel_path];
  $: raw = cache[node.rel_path];
  $: loading = raw === 'loading';
  $: children = Array.isArray(raw) ? raw : [];
</script>

{#if node.is_dir}
  <div class="dir">
    <div class="tree-row">
      <button
        class="toggle"
        on:click={() => onToggle(node)}
        aria-expanded={isOpen}
        aria-label={isOpen ? '收起' : '展开'}
      >
        <Icon name={isOpen ? 'chevron_down' : 'chevron_right'} size={14} />
      </button>
      <Icon name={isOpen ? 'folder-open' : 'folder'} size={14} />
      <span class="name">{node.name}</span>
      <span class="badge badge-info nowrap">{node.file_count} 文件</span>
      <span class="badge nowrap">{node.dir_count} 文件夹</span>
      <span class="size">{fmtBytes(node.total_bytes)}</span>
      <button class="btn btn-sm btn-soft" on:click={() => onRestoreDir(node)} disabled={busy}>
        恢复
      </button>
    </div>

    {#if isOpen}
      <div class="children">
        {#if loading}
          <div class="tree-state">
            <span class="spin"></span>加载中…
          </div>
        {:else if children.length === 0}
          <div class="tree-state">（空文件夹）</div>
        {:else}
          {#each children as child (child.rel_path)}
            <svelte:self
              node={child}
              {expanded}
              {cache}
              {busy}
              {onToggle}
              {onRestoreFile}
              {onRestoreDir}
            />
          {/each}
        {/if}
      </div>
    {/if}
  </div>
{:else}
  <div class="tree-row">
    <span class="indent"></span>
    <Icon name="file" size={14} />
    <span class="name">{node.name}</span>
    <span class="size">{fmtBytes(node.size)}</span>
    <button class="btn btn-sm btn-ghost" on:click={() => onRestoreFile(node)} disabled={busy}>
      恢复
    </button>
  </div>
{/if}

<style>
  .tree-row {
    flex-wrap: wrap;
    gap: 6px;
  }
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
  .tree-state {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 7px 10px;
    color: var(--text-3);
    font-size: 12.5px;
  }
  .spin {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid var(--text-3);
    border-right-color: transparent;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>

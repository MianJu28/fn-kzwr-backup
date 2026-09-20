<script>
  // 操作审计日志：记录敏感/破坏性操作（凭据变更、密钥导出、配置导入、备份恢复等）
  import Icon from './Icon.svelte';
  import { fmtTime } from '../lib/format.js';

  export let entries = [];
  export let busy = false;
  export let onLoad = null; // () => Promise

  let loading = false;
  let shown = 20;

  const ACTION_TEXT = {
    'webdav.credentials': 'WebDAV 凭据',
    'keys.set': '更换私钥',
    'keys.generate': '生成密钥',
    'keys.export': '导出私钥',
    'keys.backup_ack': '私钥备份确认',
    'config.save': '保存配置',
    'config.import': '导入配置',
    'config.export': '导出配置',
    'backup.run': '备份',
    'restore.run': '恢复',
    'kzwr.token.save': '保存 token',
    'kzwr.token.clear': '清除 token',
    'kzwr.trash.empty': '清空回收站',
  };

  $: list = entries || [];
  $: visible = list.slice(0, shown);

  async function reload() {
    if (!onLoad) return;
    loading = true;
    try {
      await onLoad();
    } finally {
      loading = false;
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="file" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">操作审计</h2>
      <p class="card-desc">
        记录敏感与破坏性操作（凭据变更、密钥导出、配置导入、清空回收站、备份与恢复），
        保存在本机 <code>audit.log</code>，最多保留最近 1000 条
      </p>
    </div>
    <span class="badge">{list.length} 条</span>
  </div>

  <div class="card-body">
    {#if list.length === 0}
      <div class="empty slim">
        <div class="icon-wrap"><Icon name="file" size={19} /></div>
        <strong>暂无审计记录</strong>
        点击下方「刷新」读取
      </div>
    {:else}
      <ul class="list">
        {#each visible as e, i (e.ts + '-' + i)}
          <li class:fail={!e.ok}>
            <span class="ts mono">{fmtTime(e.ts)}</span>
            <span class="tag">{ACTION_TEXT[e.action] || e.action}</span>
            <span class="detail">{e.detail}</span>
            {#if !e.ok}
              <span class="badge badge-danger">失败</span>
            {/if}
          </li>
        {/each}
      </ul>
      {#if list.length > shown}
        <button class="btn btn-sm btn-ghost more" on:click={() => (shown += 20)}>
          <Icon name="chevron_down" size={13} />显示更多（剩余 {list.length - shown} 条）
        </button>
      {/if}
    {/if}
  </div>

  <div class="card-foot foot">
    <span class="foot-hint">仅本机可读；不记录凭据/私钥内容本身</span>
    <button class="btn btn-soft" on:click={reload} disabled={busy || loading || !onLoad}>
      {#if loading}<span class="spin"></span>读取中…{:else}<Icon name="refresh" size={14} />刷新{/if}
    </button>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .slim {
    padding: 14px;
    font-size: 12.5px;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 5px;
    max-height: 420px;
    overflow-y: auto;
  }
  .list li {
    display: flex;
    align-items: baseline;
    gap: 9px;
    flex-wrap: wrap;
    padding: 7px 10px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    border: 1px solid var(--border);
    font-size: 12px;
  }
  .list li.fail {
    border-color: var(--danger-border);
    background: var(--danger-soft);
  }
  .ts {
    flex-shrink: 0;
    color: var(--text-3);
    font-size: 11px;
  }
  .tag {
    flex-shrink: 0;
    font-weight: 600;
    color: var(--text-2);
  }
  .detail {
    flex: 1;
    min-width: 0;
    color: var(--text-2);
    word-break: break-word;
  }
  .more {
    margin-top: var(--s2);
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s3);
    flex-wrap: wrap;
  }
  .foot-hint {
    color: var(--text-3);
    font-size: 12px;
  }
  .spin {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid currentColor;
    border-right-color: transparent;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>

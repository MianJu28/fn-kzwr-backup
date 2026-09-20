<script>
  // 一键体检：逐项检查 WebDAV/密钥/路径/定时/增强功能/空间，并给出修复建议
  import Icon from './Icon.svelte';

  export let result = null; // { items, ok_count, warn_count, fail_count, version }
  export let busy = false;
  export let onCheck = null; // () => Promise
  export let onGoto = null; // (pageId) => void

  let running = false;

  const ICONS = {
    ok: 'check-circle',
    warn: 'alert',
    fail: 'x-circle',
    skip: 'info',
  };
  const PAGE_OF = {
    webdav: 'settings',
    key: 'settings',
    kzwr: 'settings',
    quota: 'settings',
    paths: 'backup',
    schedule: 'backup',
  };

  $: items = (result && result.items) || [];
  $: failCount = result ? result.fail_count : 0;
  $: warnCount = result ? result.warn_count : 0;
  $: okCount = result ? result.ok_count : 0;
  $: badgeClass = failCount > 0 ? 'badge-danger' : warnCount > 0 ? 'badge-warn' : 'badge-ok';
  $: badgeText = !result
    ? '未检查'
    : failCount > 0
      ? `${failCount} 项需处理`
      : warnCount > 0
        ? `${warnCount} 项建议完善`
        : '全部通过';

  async function run() {
    if (!onCheck) return;
    running = true;
    try {
      await onCheck();
    } finally {
      running = false;
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="shield" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">一键体检</h2>
      <p class="card-desc">
        检查 WebDAV 连通性、私钥备份、备份路径、定时任务与增强功能，给出可操作建议
      </p>
    </div>
    <span class="badge {badgeClass}">{badgeText}</span>
  </div>

  {#if items.length}
    <div class="card-body">
      <ul class="checks">
        {#each items as it (it.key)}
          <li class={it.status}>
            <Icon name={ICONS[it.status] || 'info'} size={15} />
            <div class="text">
              <div class="line">
                <span class="title">{it.title}</span>
                <span class="detail">{it.detail}</span>
              </div>
              {#if it.hint}
                <div class="hint">
                  <Icon name="arrow-right" size={12} />{it.hint}
                  {#if PAGE_OF[it.key] && onGoto}
                    <button
                      class="btn btn-sm btn-ghost inline"
                      on:click={() => onGoto(PAGE_OF[it.key])}
                    >
                      前往处理
                    </button>
                  {/if}
                </div>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  <div class="card-foot foot">
    <span class="foot-hint">
      {#if result}
        {okCount} 项通过 · {warnCount} 项建议 · {failCount} 项异常
      {:else}
        首次检查会实际连接 WebDAV 与酷族接口，约需数秒
      {/if}
    </span>
    <button class="btn btn-primary" on:click={run} disabled={busy || running || !onCheck}>
      {#if running}<span class="spin"></span>检查中…{:else}<Icon name="check" size={15} />开始检查{/if}
    </button>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .checks {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .checks li {
    display: flex;
    gap: 9px;
    padding: 9px 11px;
    border-radius: var(--r-sm);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text-2);
  }
  .checks li.ok {
    border-color: var(--success-border);
    background: var(--success-soft);
    color: var(--success);
  }
  .checks li.warn {
    border-color: var(--warn-border);
    background: var(--warn-soft);
    color: var(--warn);
  }
  .checks li.fail {
    border-color: var(--danger-border);
    background: var(--danger-soft);
    color: var(--danger);
  }
  .text {
    flex: 1;
    min-width: 0;
  }
  .line {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: baseline;
  }
  .title {
    font-weight: 600;
    font-size: 12.5px;
    flex-shrink: 0;
  }
  .detail {
    font-size: 12.5px;
    color: var(--text-2);
    word-break: break-word;
  }
  .hint {
    margin-top: 4px;
    font-size: 12px;
    color: var(--text-3);
    display: flex;
    align-items: center;
    gap: 5px;
    flex-wrap: wrap;
  }
  .inline {
    padding: 1px 7px;
    height: auto;
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

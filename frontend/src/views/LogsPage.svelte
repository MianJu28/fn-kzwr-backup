<script>
  // 运行日志页：页面自持数据（进入即加载，避免经 App 状态中转的更新问题）
  import { onMount } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';
  import { fmtBytes } from '../lib/format.js';

  export let debug = false;
  export let onSaveDebug = null; // (enabled) => Promise<{error?}>

  const TAIL = 800;

  let lines = [];
  let truncated = false;
  let size = 0;
  /** 加载失败原因（与「确实没有日志」区分开）
   *
   * ⚠️ 这个声明不能省：`load()` 里先执行 `loadErr = ''`，若变量未声明，
   * 在 ES module（严格模式）下会抛 `ReferenceError`，且抛出点在 `try` **之外**，
   * `finally` 也不会执行 ⇒ `loading` 永远停在 true，页面永久卡在「刷新中」，
   * 请求一个都不会发出。vite 构建对这种「未声明赋值」只报 warning，不失败。
   */
  let loadErr = '';
  let loading = false;
  let clearing = false;
  let savingDebug = false;

  // 倒序显示：最新日志在最上方（文件本身仍是追加写的正序，下载保持原样）
  $: text = [...lines].reverse().join('\n');

  /** 调试日志开关（切换即保存并热生效） */
  async function toggleDebug(e) {
    const el = e.currentTarget;
    const want = el.checked;
    if (!onSaveDebug) return;
    savingDebug = true;
    try {
      const r = await onSaveDebug(want);
      if (r && r.error) {
        // ⚠️ **必须回滚复选框**：`checked={debug}` 是单向属性，
        // `debug` prop 没变时 Svelte 不会重写 DOM，于是复选框停在用户拨到的位置，
        // 用户以为已开启调试日志 —— 实际后端返回了错误。
        el.checked = !want;
        toast.error(r.error);
        return;
      }
      toast.success(want ? '已开启调试日志：将记录详细请求/响应日志' : '已关闭调试日志');
    } catch (err) {
      el.checked = !want; // 网络异常同样回滚
      toast.error(String(err && err.message ? err.message : err));
    } finally {
      savingDebug = false;
    }
  }

  async function load() {
    loading = true;
    loadErr = '';
    try {
      const d = await api.logsTail(TAIL);
      lines = d.lines || [];
      truncated = !!d.truncated;
      size = d.size || 0;
      // 业务错误（如日志文件不可读）也记下来：否则界面会显示「暂无日志」，
      // 把「读不出来」误报成「确实没有」。
      if (d.error) {
        loadErr = d.error;
        toast.error(d.error);
      }
    } catch (e) {
      loadErr = e.message;
      toast.error(e.message, '读取日志失败');
    } finally {
      loading = false;
    }
  }

  async function clearLogs() {
    const yes = await confirmDialog({
      title: '清空运行日志？',
      message: '日志文件将被清空（建议先下载留存）。调试日志开关不受影响。',
      confirmText: '清空',
      danger: true,
    });
    if (!yes) return;
    clearing = true;
    try {
      const d = await api.logsClear();
      if (d && d.error) toast.error(d.error);
      else {
        toast.success('运行日志已清空');
        await load();
      }
    } catch (e) {
      toast.error(e.message);
    } finally {
      clearing = false;
    }
  }

  function download() {
    window.open(api.logsDownloadUrl, '_blank');
  }

  onMount(load);
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="file" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">运行日志</h2>
      <p class="card-desc">末尾 {TAIL} 行 · 共 {fmtBytes(size)} · <strong>最新在上</strong> · 宿主本地时区</p>
    </div>
    {#if truncated}
      <span class="badge badge-warn" title="仅显示日志末尾">已截断</span>
    {/if}
    <button class="btn btn-sm btn-soft nowrap" on:click={load} disabled={loading}>
      {#if loading}<span class="spin"></span>刷新中{:else}<Icon name="refresh" size={13} />刷新{/if}
    </button>
  </div>

  <div class="card-body dbg-body">
    <label class="dbg-row">
      <input
        type="checkbox"
        checked={debug}
        on:change={toggleDebug}
        disabled={savingDebug || !onSaveDebug}
      />
      <span class="dbg-text">
        调试日志
        <small>记录网络请求/响应明细与关键流程，便于问题定位；切换后立即生效并持久化（关闭后恢复常规日志）</small>
      </span>
      {#if savingDebug}<span class="spin"></span>{/if}
    </label>
  </div>

  <div class="card-body">
    {#if !loading && lines.length === 0}
      <div class="empty slim">
        <div class="icon-wrap"><Icon name="file" size={19} /></div>
        <strong>暂无日志</strong>
      </div>
    {:else}
      <pre class="logview mono">{text}</pre>
    {/if}
  </div>

  <div class="card-foot foot">
    <button class="btn btn-soft" on:click={download} disabled={loading}>
      <Icon name="download" size={14} />下载日志
    </button>
    <button class="btn btn-ghost danger" on:click={clearLogs} disabled={loading || clearing}>
      {#if clearing}<span class="spin"></span>清空中{:else}<Icon name="trash" size={14} />清空日志{/if}
    </button>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .dbg-body {
    padding-top: 0;
  }
  .dbg-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    cursor: pointer;
  }
  .dbg-row input {
    margin-top: 2px;
    accent-color: var(--primary);
  }
  .dbg-text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 13px;
    color: var(--text);
  }
  .dbg-text small {
    color: var(--text-3);
    font-size: 12px;
  }
  .foot {
    display: flex;
    justify-content: flex-end;
    gap: var(--s2);
  }
  .logview {
    margin: 0;
    padding: 10px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    font-size: 11.5px;
    line-height: 1.55;
    max-height: 60vh;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--text-2);
  }
  .spin {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid currentColor;
    border-right-color: transparent;
    animation: spin 0.7s linear infinite;
    display: inline-block;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>

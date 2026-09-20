<script>
  // kzwr REST 增强功能（可选）：access-token、账号详情与空间、回收站、空间预警
  //
  // 备份/恢复仍然走官方 WebDAV（Basic 认证）；这里的 access-token 仅用于
  // WebDAV 提供不了的账号级能力（存储空间/套餐、回收站）。token 属于凭据，
  // 后端永不回显，只返回「是否已配置」。
  import Icon from './Icon.svelte';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';
  import { fmtBytes } from '../lib/format.js';

  export let configured = false; // 是否已配置 access-token
  export let user = null; // 账号信息（含空间与详情）
  export let quotaWarnPercent = 85; // 空间占用预警阈值（%，0 = 关闭）
  export let busy = false;
  export let onSaveToken = null; // (token) => Promise<{success, configured, warning, error}>
  export let onSaveQuota = null; // (percent) => Promise<{error?}>
  export let onEmptyTrash = null; // () => Promise<{emptied, kept, total_bytes, reason, error}>

  let token = '';
  let show = false;
  let working = false;
  let clearing = false;
  let msg = '';
  let msgOk = false;
  let warned = '';
  let trashMsg = '';
  let qwPercent = 85;
  let savingQuota = false;
  let quotaMsg = '';

  $: hasQuota = !!(user && user.total);
  $: pct = hasQuota ? Math.min(100, Math.round((user.used / user.total) * 100)) : 0;
  $: overWarn = hasQuota && quotaWarnPercent > 0 && pct >= quotaWarnPercent;

  // 仅在「后端回显值变化」时同步门槛输入，避免覆盖用户正在输入的值
  let lastQw = null;
  function syncQw(v) {
    const n = Number(v) || 0;
    if (n === lastQw) return;
    lastQw = n;
    qwPercent = n;
  }
  $: syncQw(quotaWarnPercent);

  async function save() {
    if (!token.trim()) {
      msg = '请先粘贴 access-token';
      msgOk = false;
      return;
    }
    working = true;
    msg = '';
    warned = '';
    const r = await onSaveToken(token.trim());
    working = false;
    if (r && r.success) {
      token = '';
      warned = r.warning || '';
      msg = warned ? '已保存并通过校验（存在账号不一致提醒，见下）' : '已保存并通过校验，增强功能已启用';
      msgOk = true;
      toast.success(warned ? '已保存，请查看账号提醒' : '增强功能已启用');
    } else {
      msg = (r && r.error) || '保存失败';
      msgOk = false;
      toast.error(msg);
    }
  }

  async function saveQuota() {
    if (!onSaveQuota) return;
    savingQuota = true;
    quotaMsg = '';
    const n = Math.max(0, Math.min(100, Math.floor(Number(qwPercent) || 0)));
    const r = await onSaveQuota(n);
    savingQuota = false;
    if (r && r.error) {
      quotaMsg = r.error;
      toast.error(r.error);
    } else {
      qwPercent = n;
      quotaMsg = n === 0 ? '已关闭空间预警' : `已保存，占用达到 ${n}% 时告警`;
      toast.success(quotaMsg);
    }
  }

  async function clear() {
    const yes = await confirmDialog({
      title: '清除 access-token？',
      message: '清除后将无法查看存储空间与清理回收站；不影响备份与恢复。',
      confirmText: '清除',
      danger: true,
    });
    if (!yes) return;
    working = true;
    msg = '';
    warned = '';
    const r = await onSaveToken('');
    working = false;
    msg = r && r.success ? '已清除 access-token' : (r && r.error) || '清除失败';
    msgOk = !!(r && r.success);
  }

  async function emptyTrash() {
    const yes = await confirmDialog({
      title: '清空云端回收站？',
      message: '回收站内的文件将被永久删除且无法恢复。确定继续？',
      confirmText: '永久删除',
      danger: true,
    });
    if (!yes) return;
    clearing = true;
    trashMsg = '';
    const r = await onEmptyTrash();
    clearing = false;
    if (r && r.error) {
      trashMsg = r.error;
      toast.error(r.error);
    } else {
      trashMsg = r.emptied
        ? `已清空 ${r.emptied} 项（占用约 ${fmtBytes(r.total_bytes || 0)}）`
        : `未删除任何条目：${r.reason || '回收站为空'}`;
      toast.success(trashMsg);
    }
  }
</script>

<section class="card">
  <div class="card-head">
    <div class="icon-wrap"><Icon name="wifi" size={18} /></div>
    <div class="grow">
      <h2 class="card-title">增强功能（可选）</h2>
      <p class="card-desc">
        仅用于官方 WebDAV 提供不了的账号级能力：<strong>存储空间/账号详情</strong>、<strong>回收站清理</strong>、
        <strong>空间预警</strong>。不影响备份与恢复（它们始终走 WebDAV）
      </p>
    </div>
    <span class="badge {configured ? 'badge-ok' : ''}">{configured ? '已启用' : '未启用'}</span>
  </div>

  <div class="card-body">
    {#if hasQuota}
      <div class="stat-grid">
        <div class="stat">
          <div class="stat-label"><Icon name="hard_drive" size={12} />已用空间</div>
          <div class="stat-value">{fmtBytes(user.used)}</div>
          <div class="stat-sub">共 {fmtBytes(user.total)}{user.percentage ? ` · ${user.percentage}` : ''}</div>
        </div>
        <div class="stat">
          <div class="stat-label"><Icon name="user" size={12} />账号</div>
          <div class="stat-value small">{user.name || user.email || '—'}</div>
          <div class="stat-sub">{user.plan || '套餐未知'}</div>
        </div>
      </div>
      <div class="bar" class:over={overWarn} aria-label="空间占用 {pct}%">
        <div class="fill" style="width:{pct}%"></div>
      </div>

      <div class="details">
        {#if user.email}<span class="kv"><b>邮箱</b>{user.email}</span>{/if}
        {#if user.uid !== null && user.uid !== undefined}<span class="kv"><b>UID</b>{user.uid}</span>{/if}
        {#if user.max_file_size}<span class="kv"><b>单文件上限</b>{fmtBytes(user.max_file_size)}</span>{/if}
        {#if user.country}<span class="kv"><b>地区</b>{user.country}</span>{/if}
        {#if user.ip}<span class="kv"><b>最近 IP</b>{user.ip}</span>{/if}
        {#if user.language}<span class="kv"><b>语言</b>{user.language}</span>{/if}
        <span class="kv"><b>家庭组</b>{user.in_family ? '是' : '否'}</span>
        {#if user.upgrading}<span class="kv warn"><b>状态</b>升级中</span>{/if}
        {#if quotaWarnPercent > 0}
          <span class="kv"><b>空间预警</b>{quotaWarnPercent}%</span>
        {/if}
      </div>

      {#if user.announcement}
        <div class="alert alert-info">
          <Icon name="bell" size={15} />
          <div class="alert-body">酷族公告：{user.announcement}</div>
        </div>
      {/if}
      {#if overWarn}
        <div class="alert alert-warn">
          <Icon name="alert" size={15} />
          <div class="alert-body">
            云端空间已用 {pct}%，达到预警阈值 {quotaWarnPercent}%，请清理回收站或扩容，以免备份失败。
          </div>
        </div>
      {/if}
    {:else if user && user.error}
      <div class="alert alert-warn">
        <Icon name="alert" size={15} />
        <div class="alert-body">{user.error}</div>
      </div>
    {/if}

    {#if warned}
      <div class="alert alert-warn">
        <Icon name="alert" size={15} />
        <div class="alert-body">{warned}</div>
      </div>
    {/if}

    <label class="field token-field">
      <span class="label">
        access-token <span class="opt">（从浏览器 Cookie 获取；仅保存在本机，age 加密）</span>
      </span>
      <div class="token-row">
        <input
          class="input mono"
          type={show ? 'text' : 'password'}
          value={token}
          on:input={(e) => (token = e.currentTarget.value)}
          placeholder={configured ? '已保存（如需更换请粘贴新值）' : '粘贴 access-token'}
          autocomplete="off"
          spellcheck="false"
        />
        <button
          class="btn-icon reveal"
          type="button"
          on:click={() => (show = !show)}
          aria-label={show ? '隐藏' : '显示'}
          title={show ? '隐藏' : '显示'}
        >
          <Icon name={show ? 'eye-off' : 'eye'} size={15} />
        </button>
      </div>
    </label>

    <div class="quota-row">
      <label class="field quota-field">
        <span class="label">空间预警阈值（%）<span class="opt">0 = 关闭</span></span>
        <span class="quota-inline">
          <input class="input mono" type="number" min="0" max="100" bind:value={qwPercent} />
          <button
            class="btn btn-soft nowrap"
            on:click={saveQuota}
            disabled={busy || savingQuota || !onSaveQuota}
          >
            {#if savingQuota}<span class="spin"></span>保存中{:else}<Icon name="check" size={14} />保存阈值{/if}
          </button>
        </span>
      </label>
      {#if quotaMsg}<span class="quota-msg">{quotaMsg}</span>{/if}
    </div>

    <details class="guide">
      <summary>如何获取 access-token？（有效期有限，失效会告警提醒）</summary>
      <ol>
        <li>浏览器新标签打开 <code>https://www.kzwr.com</code> 并<strong>登录</strong>你的账号</li>
        <li>按 <code>F12</code> 打开开发者工具 → <code>Application</code>（应用/存储）</li>
        <li>左侧 <code>Cookies</code> → <code>https://www.kzwr.com</code></li>
        <li>找到名为 <code>access-token</code> 的那一行，双击 <code>Value</code> 全选复制</li>
        <li>粘贴到上面的输入框 → 点「保存并校验」（会实时验证有效性）</li>
      </ol>
      <p class="guide-note">
        access-token 会随退出登录或过期失效：本应用在使用时会自动校验，一旦失效会生成告警提醒你重新复制。
      </p>
    </details>

    {#if msg}
      <div class="alert {msgOk ? 'alert-ok' : 'alert-danger'} msg">
        <Icon name={msgOk ? 'check-circle' : 'x-circle'} size={15} />
        <div class="alert-body">{msg}</div>
      </div>
    {/if}
    {#if trashMsg}
      <div class="alert alert-info msg">
        <Icon name="info" size={15} />
        <div class="alert-body">{trashMsg}</div>
      </div>
    {/if}
  </div>

  <div class="card-foot foot">
    {#if configured}
      <button class="btn btn-soft" on:click={emptyTrash} disabled={busy || clearing || working}>
        {#if clearing}<span class="spin"></span>清理中…{:else}<Icon name="trash" size={14} />清空回收站{/if}
      </button>
      <button class="btn btn-ghost" on:click={clear} disabled={busy || working || clearing}>
        清除 token
      </button>
    {/if}
    <button class="btn btn-primary" on:click={save} disabled={busy || working || clearing || !token.trim()}>
      {#if working}<span class="spin"></span>校验中…{:else}<Icon name="check" size={15} />保存并校验{/if}
    </button>
  </div>
</section>

<style>
  .grow {
    flex: 1;
    min-width: 0;
  }
  .stat-value.small {
    font-size: 15px;
  }
  .bar {
    height: 7px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
    margin-top: var(--s3);
  }
  .bar .fill {
    height: 100%;
    background: var(--primary);
    transition: width var(--t-fast);
  }
  .bar.over .fill {
    background: var(--warn, #d97706);
  }
  .details {
    display: flex;
    flex-wrap: wrap;
    gap: 6px var(--s4);
    margin-top: var(--s3);
  }
  .kv {
    font-size: 12px;
    color: var(--text-3);
    display: inline-flex;
    gap: 5px;
  }
  .kv b {
    color: var(--text-2);
    font-weight: 560;
  }
  .kv.warn {
    color: var(--warn);
  }
  .token-field {
    margin-top: var(--s4);
  }
  .token-row {
    display: flex;
    align-items: center;
    gap: var(--s2);
  }
  .token-row .input {
    flex: 1;
    min-width: 0;
  }
  .quota-row {
    display: flex;
    align-items: center;
    gap: var(--s3);
    margin-top: var(--s3);
    flex-wrap: wrap;
  }
  .quota-field {
    max-width: 340px;
    flex: 1;
    min-width: 240px;
  }
  /* 输入框与保存按钮同行对齐（修复按钮错位） */
  .quota-inline {
    display: flex;
    align-items: center;
    gap: var(--s2);
    width: 100%;
  }
  .quota-inline .input {
    flex: 1;
    min-width: 0;
  }
  .quota-msg {
    font-size: 12px;
    color: var(--text-3);
  }
  .guide {
    margin-top: var(--s3);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    padding: 9px 12px;
    background: var(--surface-2);
  }
  .guide summary {
    cursor: pointer;
    font-size: 12.5px;
    font-weight: 560;
    color: var(--text-2);
  }
  .guide ol {
    margin: var(--s2) 0 0;
    padding-left: 20px;
    color: var(--text-2);
    font-size: 12.5px;
    line-height: 1.85;
  }
  .guide code {
    font-family: var(--mono);
    font-size: 11.5px;
    background: var(--surface-3);
    padding: 1px 5px;
    border-radius: var(--r-xs);
  }
  .guide-note {
    margin: var(--s2) 0 0;
    font-size: 12px;
    color: var(--text-3);
  }
  .msg {
    margin-top: var(--s3);
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--s2);
    flex-wrap: wrap;
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

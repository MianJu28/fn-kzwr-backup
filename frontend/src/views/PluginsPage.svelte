<script>
  /**
   * 插件页
   *
   * 插件化后的**一等公民页面**：集中展示与管理全部插件能力。
   *
   * 结构：
   *   1. 插件卡片区 —— 由 `/api/plugins` 驱动，卡片只作**概览**
   *      （名称/来源/状态 + 「设置」「启用/停用」按钮）
   *   2. 外置插件管理 —— 开关 / 目录 / 签名公钥 / 诊断 / 卸载（`PluginSection`）
   *
   * 交互要点：
   * - **设置独立弹窗**（`PluginSettingsModal`）：内容由插件自己声明的 `ui.blocks` 渲染
   *   （方案 A：schema 驱动），宿主不写插件专用组件。
   * - **启用/停用**运行时生效，无需重启；停用是**逻辑摘除**（不卸载 `.so`）。
   *   若该插件仍被任务引用，后端会级联停用那些任务并回报 `affected_tasks`；
   *   若正在执行备份的任务用它，后端会**拒绝**并给出提示。
   */
  import Icon from '../components/Icon.svelte';
  import PluginSection from '../components/PluginSection.svelte';
  import PluginSettingsModal from '../components/PluginSettingsModal.svelte';
  import { api } from '../lib/api.js';
  import { toast } from '../lib/toast.js';
  import { confirmDialog } from '../lib/confirm.js';
  import { sectionsFor, FALLBACK_SECTIONS } from '../lib/plugins.js';

  /** 插件清单（来自 /api/plugins） */
  export let plugins = [];
  /** 插件通用区块操作完成后的回调（如 token 保存后刷新账号信息） */
  export let onPluginDone = null;

  // 外置插件加载（动态库，ADR-013 方案 B）
  export let pluginsEnabled = false;
  /** 是否由环境变量放行未签名插件（只读，仅本机调试） */
  export let pluginsAllowUnsigned = false;
  export let onSavePlugins = null; // (enabled) => Promise<{error?}>

  export let busy = false;

  /** 正在配置的插件（null = 弹窗关闭） */
  let editing = null;
  /** 正在切换启停的插件 id（按钮 loading 用） */
  let toggling = null;

  // 插件卡片：顺序与组成由后端决定；接口不可用时退回内置兜底，保证页面始终可用
  $: pluginSections = sectionsFor(
    plugins && plugins.length ? plugins : FALLBACK_SECTIONS,
    'settings'
  );

  /**
   * 汇总插件的只读概览信息（供卡片主体渲染）
   *
   * 卡片只展示 `metric` / `tips`；可编辑项（text/number/toggle/button/accounts）一律进
   * 设置弹窗，避免卡片被表单撑长、多个插件互相淹没。
   */
  /**
   * 动态指标的实时值：`插件 id + action 路径` → `{value, hint}`
   *
   * `metric` 块声明里的 `value` 只是**静态占位文案**（插件不知道自己持久化过什么，
   * 更不知道远端账号的实时用量）。卡片要显示真实数字就必须按声明去 GET ——
   * 与设置弹窗（`PluginBlocks.loadMetrics`）同一份契约，因此任何插件声明
   * `metric.action` 都能直接活起来，前端不需要认识任何插件。
   */
  let liveMetrics = {};

  async function loadLiveMetrics(list) {
    const jobs = [];
    for (const p of list || []) {
      // 停用的插件路由是 404（宿主按 is_disabled 拦），不必发请求
      if (p.disabled) continue;
      if (!p.api_base) continue;
      for (const b of (p.ui && p.ui.blocks) || []) {
        if (b.type !== 'metric' || !b.action) continue;
        const path = b.action.startsWith('/') ? b.action : `/${b.action}`;
        const key = `${p.id}|${b.action}`;
        jobs.push(
          api
            .pluginGet(p.api_base, path)
            .then((r) => {
              if (r && !r.error && r.value) {
                liveMetrics = {
                  ...liveMetrics,
                  [key]: { value: String(r.value), hint: r.hint ? String(r.hint) : '' },
                };
              } else if (r && r.error) {
                // 读取失败如实显示（例如「未配置账号」），但保留兜底文案
                liveMetrics = {
                  ...liveMetrics,
                  [key]: { value: String(r.value || '读取失败'), hint: String(r.error) },
                };
              }
            })
            .catch(() => {
              /* 网络失败：保留静态文案，不打断渲染 */
            }),
        );
      }
    }
    if (jobs.length) await Promise.all(jobs);
  }

  // 清单变化时拉一次实时值（弹窗内操作完成后由 onPluginDone 触发父级重取）
  $: if (plugins && plugins.length) loadLiveMetrics(plugins);

  function pluginStats(p) {
    const blocks = (p.ui && p.ui.blocks) || [];
    return {
      metrics: blocks.filter((b) => b.type === 'metric'),
      tips: blocks
        .filter((b) => b.type === 'tips')
        .map((b) => b.text)
        .join(' '),
      count: blocks.filter((b) =>
        ['text', 'number', 'toggle', 'button', 'accounts'].includes(b.type)
      ).length,
    };
  }

  /** 打开某插件的设置弹窗（从最新清单取，避免用过期的对象快照） */
  function openSettings(p) {
    editing = plugins.find((x) => x.id === p.id) || p;
  }

  /**
   * 启用/停用插件
   *
   * 停用可能级联停用引用它的任务，或被执行中的备份阻止 —— 两种都由后端判定；
   * 前端负责：① 停用前让用户确认；② 如实转达后端结果。
   */
  async function toggleEnabled(p) {
    const next = !p.disabled;
    if (!next) {
      const yes = await confirmDialog({
        title: `停用插件「${p.ui?.title || p.name || p.id}」？`,
        message:
          '停用后该插件的接口与卡片立即失效（无需重启）。\n' +
          '若仍有任务使用它，那些任务会被一并停用，避免备份直接失败。\n\n' +
          '注意：停用只把插件从系统中摘除，其代码仍保留在内存中；真正释放需重启应用。',
        confirmText: '停用',
        danger: true,
      });
      if (!yes) return;
    }
    toggling = p.id;
    try {
      const r = await api.pluginSetEnabled(p.id, next);
      if (r && r.error) {
        // 典型：正在执行备份的任务用了该插件 → 后端拒绝
        toast.error(r.error, '操作未生效');
        return;
      }
      const affected = (r && r.affected_tasks) || [];
      if (affected.length) {
        toast.warn(
          `已同时停用 ${affected.length} 个使用它的任务：${affected.join('、')}`,
          '插件已停用'
        );
      } else {
        toast.success(next ? '插件已启用，立即生效' : '插件已停用，立即生效');
      }
      if (onPluginDone) await onPluginDone();
    } catch (e) {
      toast.error(e.message);
    } finally {
      toggling = null;
    }
  }
</script>

<!-- ── 插件卡片区（概览 + 设置/启停入口）──────────────────────── -->
{#if pluginSections.length === 0}
  <section class="card">
    <div class="card-body">
      <div class="empty">
        <div class="icon-wrap"><Icon name="package" size={20} /></div>
        <strong>没有可用插件</strong>
        插件清单尚未加载完成，或所有插件都未加载。可在下方开启外置插件加载后重启应用。
      </div>
    </div>
  </section>
{/if}

<div class="plugin-grid">
  {#each pluginSections as p (p.id)}
    {@const ps = pluginStats(p)}
    <article class="plugin-card" class:off={p.disabled}>
      <!-- 顶部：图标 + 标题 + 状态；右侧一个悬浮的启停开关 -->
      <header class="pc-head">
        <span class="pc-icon" class:ok={!p.disabled && p.available} class:off={p.disabled}>
          <Icon name={p.kind === 'target' ? 'cloud' : 'zap'} size={19} />
        </span>
        <div class="pc-title-wrap">
          <h3 class="pc-title">{p.ui?.title || p.name || p.id}</h3>
          <div class="pc-meta">
            <code>{p.id}</code>
            <span class="dot-sep">·</span>
            <span>{p.source === 'external' ? '外置' : '内置'}</span>
            <span class="dot-sep">·</span>
            <span>{p.kind === 'target' ? '备份目标' : '增强能力'}</span>
          </div>
        </div>
        <span class="pc-state" class:on={!p.disabled && p.available} class:off={p.disabled}>
          {p.disabled ? '已停用' : p.available ? '运行中' : '待配置'}
        </span>
      </header>

      <!-- 主体：只读概览（指标 / 提示） -->
      <div class="pc-body">
        {#if ps.metrics.length}
          <div class="pc-metrics">
            {#each ps.metrics as m, i (i)}
              {@const live = m.action ? liveMetrics[`${p.id}|${m.action}`] : null}
              <div class="pc-metric">
                <span class="pc-metric-k">{m.label}</span>
                <span class="pc-metric-v">{(live && live.value) || m.value || '—'}</span>
                {#if (live && live.hint) || m.hint}
                  <span class="pc-metric-h">{(live && live.hint) || m.hint}</span>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
        {#if ps.tips}
          <p class="pc-tip">{ps.tips}</p>
        {/if}
        {#if !ps.metrics.length && !ps.tips}
          <p class="pc-tip muted">该插件未声明概览信息。</p>
        {/if}
      </div>

      <!-- 底部操作条 -->
      <footer class="pc-foot">
        <span class="pc-items">
          {#if ps.count}<Icon name="sliders" size={13} />{ps.count} 项设置{/if}
        </span>
        <div class="pc-actions">
          <button class="btn btn-sm" on:click={() => openSettings(p)} disabled={busy || p.disabled}>
            <Icon name="sliders" size={14} />设置
          </button>
          <button
            class="btn btn-sm {p.disabled ? 'btn-primary' : 'btn-ghost danger'}"
            on:click={() => toggleEnabled(p)}
            disabled={busy || toggling === p.id}
          >
            {#if toggling === p.id}
              <span class="spin"></span>
            {:else if p.disabled}
              <Icon name="zap" size={14} />启用
            {:else}
              <Icon name="x" size={14} />停用
            {/if}
          </button>
        </div>
      </footer>
    </article>
  {/each}
</div>

<!-- ── 外置插件（动态库）管理 ─────────────────────────────────── -->
<PluginSection
  enabled={pluginsEnabled}
  allowUnsigned={pluginsAllowUnsigned}
  {busy}
  onSave={onSavePlugins}
/>

<!-- ── 插件设置弹窗（内容由插件自己声明）────────────────────────── -->
<PluginSettingsModal
  plugin={editing}
  onDone={onPluginDone}
  onClose={() => (editing = null)}
/>

<style>
  /* ── 插件卡片网格 ────────────────────────────────────────────────
     自适应多列：宽屏并排、窄屏单列。卡片本身不套用全局 .card，
     以便实现「图标 + 状态胶囊 + 指标块 + 底部操作条」的紧凑版式。 */
  .plugin-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
    gap: var(--s4);
  }

  .plugin-card {
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-lg);
    box-shadow: var(--sh-1);
    overflow: hidden;
    transition:
      box-shadow 180ms ease,
      transform 180ms ease,
      border-color 180ms ease;
  }
  .plugin-card:hover {
    box-shadow: var(--sh-2);
    transform: translateY(-1px);
    border-color: var(--border-strong);
  }
  /* 停用：整卡降饱和 + 降透明度，一眼可辨 */
  .plugin-card.off {
    opacity: 0.68;
    background: var(--surface-2);
  }
  .plugin-card.off:hover {
    transform: none;
    box-shadow: var(--sh-1);
  }

  /* 顶部：左侧一条品牌色渐变条，强化「这是一个功能模块」的感觉 */
  .pc-head {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: var(--s3);
    padding: var(--s4) var(--s4) var(--s3);
  }
  .pc-head::before {
    content: '';
    position: absolute;
    inset: 0 0 auto;
    height: 3px;
    background: linear-gradient(90deg, var(--primary), #8b5cf6 60%, transparent);
    opacity: 0.9;
  }
  .plugin-card.off .pc-head::before {
    background: var(--border-strong);
  }

  .pc-icon {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    flex-shrink: 0;
    border-radius: var(--r-md);
    background: var(--surface-3);
    color: var(--text-3);
    border: 1px solid var(--border);
  }
  .pc-icon.ok {
    background: var(--primary-soft);
    color: var(--primary);
    border-color: var(--primary-soft-border);
  }
  .pc-icon.off {
    background: var(--surface-3);
    color: var(--text-3);
  }

  .pc-title-wrap {
    flex: 1;
    min-width: 0;
  }
  .pc-title {
    margin: 0;
    font-size: 14.5px;
    font-weight: 660;
    line-height: 1.35;
    letter-spacing: -0.01em;
  }
  .pc-meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 3px;
    font-size: 11.5px;
    color: var(--text-3);
  }
  .pc-meta code {
    font-size: 11px;
    padding: 1px 5px;
    border-radius: var(--r-xs);
    background: var(--surface-3);
    color: var(--text-2);
  }
  .dot-sep {
    opacity: 0.5;
  }

  /* 状态胶囊：运行中 = 绿点呼吸；停用 = 灰；待配置 = 琥珀 */
  .pc-state {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex-shrink: 0;
    padding: 3px 9px;
    border-radius: var(--r-full);
    font-size: 11px;
    font-weight: 600;
    background: var(--surface-3);
    color: var(--text-3);
    border: 1px solid var(--border);
  }
  .pc-state::before {
    content: '';
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
  .pc-state.on {
    background: var(--success-soft);
    color: var(--success);
    border-color: var(--success-border);
  }
  .pc-state.on::before {
    animation: breathe 2.4s ease-in-out infinite;
  }
  .pc-state.off {
    background: var(--surface-3);
    color: var(--text-3);
  }
  @keyframes breathe {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.35;
    }
  }

  /* 主体：指标块用两列网格，数值用等宽字体便于对齐 */
  .pc-body {
    flex: 1;
    padding: 0 var(--s4) var(--s4);
  }
  .pc-metrics {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: var(--s2);
  }
  .pc-metric {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 9px 11px;
    border-radius: var(--r-md);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .pc-metric-k {
    font-size: 11px;
    font-weight: 560;
    color: var(--text-3);
  }
  .pc-metric-v {
    font-size: 14px;
    font-weight: 640;
    color: var(--text);
    font-variant-numeric: tabular-nums;
    word-break: break-all;
  }
  .pc-metric-h {
    font-size: 11px;
    color: var(--text-3);
    line-height: 1.45;
  }
  .pc-tip {
    margin: var(--s2) 0 0;
    font-size: 12px;
    line-height: 1.65;
    color: var(--text-3);
  }
  .pc-tip.muted {
    opacity: 0.75;
  }

  /* 底部操作条：设置项计数在左，按钮在右 */
  .pc-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s2);
    padding: var(--s3) var(--s4);
    background: var(--surface-2);
    border-top: 1px solid var(--border);
  }
  .pc-items {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11.5px;
    color: var(--text-3);
  }
  .pc-actions {
    display: flex;
    gap: var(--s2);
  }

  /* 空状态 */
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s2);
    padding: var(--s6) var(--s4);
    text-align: center;
    color: var(--text-3);
    font-size: 13px;
  }
  .empty strong {
    color: var(--text-2);
    font-size: 14px;
  }
  .empty .icon-wrap {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border-radius: var(--r-full);
    background: var(--surface-3);
    color: var(--text-3);
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

  /* 窄屏：单列，卡片内边距收窄 */
  @media (max-width: 560px) {
    .plugin-grid {
      grid-template-columns: 1fr;
    }
    .pc-head,
    .pc-body,
    .pc-foot {
      padding-left: var(--s3);
      padding-right: var(--s3);
    }
  }
</style>

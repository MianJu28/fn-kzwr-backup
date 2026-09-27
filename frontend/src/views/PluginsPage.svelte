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
  export let pluginsDir = '';
  /** 插件签名公钥（base64 32 字节 Ed25519 公钥，每行一个） */
  export let pluginsPubkeys = [];
  /** 是否由环境变量放行未签名插件（只读，仅本机调试） */
  export let pluginsAllowUnsigned = false;
  export let onSavePlugins = null; // (enabled, dir, pubkeys) => Promise<{error?}>

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
{#each pluginSections as p (p.id)}
  <section class="card" class:is-off={p.disabled}>
    <div class="card-head">
      <div class="icon-wrap" class:off={p.disabled}><Icon name="package" size={18} /></div>
      <div class="grow">
        <h2 class="card-title">
          {p.ui?.title || p.name || p.id}
          {#if p.disabled}
            <span class="badge badge-warn">已停用</span>
          {:else if !p.available}
            <span class="badge">未启用</span>
          {:else}
            <span class="badge badge-ok">已启用</span>
          {/if}
        </h2>
        <p class="card-desc">
          <code>{p.id}</code>
          {#if p.source === 'external'}· 外置动态库{:else}· 内置{/if}
          {#if p.ui?.blocks?.length}· {p.ui.blocks.length} 个设置项{/if}
        </p>
      </div>
    </div>

    <div class="card-body">
      <!-- 概览：卡片上只放只读信息（指标/提示），可编辑项统一进弹窗 -->
      <div class="summary">
        {#each p.ui?.blocks || [] as b, i (i)}
          {#if b.type === 'metric'}
            <div class="stat">
              <div class="stat-label">{b.label}</div>
              <div class="stat-value">{b.value || '—'}</div>
              {#if b.hint}<div class="stat-sub">{b.hint}</div>{/if}
            </div>
          {:else if b.type === 'tips'}
            <p class="card-desc tips">{b.text}</p>
          {/if}
        {/each}
      </div>
      {#if !p.ui?.blocks?.length}
        <p class="card-desc">该插件暂未声明界面。</p>
      {/if}
    </div>

    <div class="card-foot foot">
      <button class="btn btn-sm" on:click={() => openSettings(p)} disabled={busy || p.disabled}>
        <Icon name="sliders" size={14} />设置
      </button>
      <button
        class="btn btn-sm {p.disabled ? 'btn-primary' : 'btn-ghost danger'}"
        on:click={() => toggleEnabled(p)}
        disabled={busy || toggling === p.id}
      >
        {#if toggling === p.id}
          <span class="spin"></span>处理中
        {:else if p.disabled}
          <Icon name="zap" size={14} />启用
        {:else}
          <Icon name="x" size={14} />停用
        {/if}
      </button>
    </div>
  </section>
{/each}

<!-- ── 外置插件（动态库）管理 ─────────────────────────────────── -->
<PluginSection
  enabled={pluginsEnabled}
  dir={pluginsDir}
  pubkeys={pluginsPubkeys}
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
  .grow {
    flex: 1;
    min-width: 0;
  }
  /* 停用的插件整卡降透明度，一眼看出不可用 */
  .card.is-off {
    opacity: 0.72;
  }
  .icon-wrap.off {
    background: var(--surface-3);
    color: var(--text-3);
  }
  .summary {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .tips {
    margin: 0;
  }
  .foot {
    display: flex;
    justify-content: flex-end;
    gap: var(--s2);
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

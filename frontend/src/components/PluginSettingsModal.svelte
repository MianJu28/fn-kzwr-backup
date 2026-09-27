<script>
  /**
   * 插件设置弹窗（方案 A：schema 驱动）
   *
   * 插件页的每张卡片都有「设置」按钮 → 打开本弹窗，内容用 `PluginBlocks` 渲染，
   * 即**完全由插件自己声明的 `ui.blocks` 决定**（metric/text/number/toggle/button/tips）。
   *
   * 为什么用弹窗而不是内联展开：
   * - 插件卡片在插件页只作概览（名称/来源/状态），设置项多了会互相淹没；
   * - 弹窗让「配置一个插件」有明确的开始/结束，避免长页面里滚动找表单。
   *
   * 为什么不做成插件自带 HTML（方案 B）：
   * 现有架构下插件只需声明 schema，宿主统一渲染 —— 外观一致、无注入面、
   * 且**外置插件无需重新打包前端**这一承诺继续成立。
   */
  import Icon from './Icon.svelte';
  import PluginBlocks from './PluginBlocks.svelte';

  /** 要配置的插件（`/api/plugins` 的一项）；null = 关闭 */
  export let plugin = null;
  /** 操作完成后的回调（让外层刷新插件清单/账号信息） */
  export let onDone = null;
  /** 关闭回调 */
  export let onClose = null;

  function close() {
    if (onClose) onClose();
  }

  function onKeydown(e) {
    if (e.key === 'Escape') close();
  }
</script>

<svelte:window on:keydown={onKeydown} />

{#if plugin}
  <div class="scrim" role="presentation" on:click|self={close}>
    <div class="modal plugin-modal" role="dialog" aria-modal="true" aria-label={`${plugin.name || plugin.id} 设置`}>
      <div class="modal-title">
        <Icon name="package" size={18} />
        <span class="grow">{plugin.ui?.title || plugin.name || plugin.id} · 设置</span>
        <button class="btn-icon" on:click={close} aria-label="关闭">
          <Icon name="x" size={16} />
        </button>
      </div>

      <div class="modal-body scroll-body">
        {#if plugin.disabled}
          <div class="alert alert-warn">
            <Icon name="alert" size={15} />
            <div class="alert-body">
              该插件已被<strong>停用</strong>，其设置暂时不可用。
              先在上方「启用」后再配置。
            </div>
          </div>
        {:else if plugin.ui && plugin.ui.blocks && plugin.ui.blocks.length}
          <!-- 插件的界面完全由它自己声明；宿主只负责渲染与调用 -->
          <PluginBlocks {plugin} onDone={onDone} variant="embedded" />
        {:else}
          <div class="empty slim">
            <div class="icon-wrap"><Icon name="info" size={18} /></div>
            <strong>该插件未声明设置项</strong>
            插件可通过 `ui.blocks` 提供设置界面（metric / text / number / toggle / button / tips）。
          </div>
        {/if}
      </div>

      <div class="modal-actions">
        <button class="btn btn-ghost" on:click={close}>关闭</button>
      </div>
    </div>
  </div>
{/if}

<style>
  /* 比通用 modal 宽一些：插件设置常含表单与多列指标 */
  .plugin-modal {
    max-width: 620px;
    width: 100%;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  /* 设置项多时内部滚动，标题与底部按钮保持可见 */
  .scroll-body {
    max-height: min(62vh, 560px);
    overflow-y: auto;
    margin-bottom: var(--s4);
    padding-right: 2px;
  }
</style>

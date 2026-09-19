<script>
  // 轻提示渲染容器（挂载于 App 根部，由 lib/toast.js 驱动）
  import { toasts, dismiss } from '../lib/toast.js';
  import Icon from './Icon.svelte';

  const ICON = { ok: 'check-circle', error: 'x-circle', warn: 'alert', info: 'info' };
</script>

{#if $toasts.length}
  <div class="toast-stack">
    {#each $toasts as t (t.id)}
      <div class="toast toast-{t.type}" role="status">
        <Icon name={ICON[t.type] || 'info'} size={18} />
        <div class="body">
          {#if t.title}<div class="toast-title">{t.title}</div>{/if}
          <div class="toast-msg">{t.message}</div>
        </div>
        <button class="toast-close" on:click={() => dismiss(t.id)} aria-label="关闭提示">
          <Icon name="x" size={14} />
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .body {
    flex: 1;
    min-width: 0;
  }
</style>

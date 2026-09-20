<script>
  // 全局确认弹窗（由 lib/confirm.js 的 confirmDialog() 驱动）
  // input 模式：显示口令/文本输入框，确认时把输入值回传给 confirmDialog 的 Promise
  import { confirmState, answerConfirm } from '../lib/confirm.js';
  import Icon from './Icon.svelte';

  let confirmBtn = null;
  let inputEl = null;
  let inputValue = '';

  $: state = $confirmState;
  $: if (state && state.input && inputEl) inputEl.focus();
  $: if (state && !state.input && confirmBtn) confirmBtn.focus();
  // 每次打开重置输入
  $: if (state) inputValue = '';

  function onKey(e) {
    if (!state) return;
    if (e.key === 'Escape') answerConfirm(false);
  }
</script>

<svelte:window on:keydown={onKey} />

{#if state}
  <div
    class="scrim"
    role="presentation"
    on:click|self={() => answerConfirm(false)}
  >
    <div class="modal" role="dialog" aria-modal="true" aria-label={state.title}>
      <div class="modal-title" class:danger={state.danger}>
        <Icon name={state.danger ? 'alert' : 'info'} size={19} />
        <span>{state.title}</span>
      </div>
      {#if state.message}
        <div class="modal-body">{state.message}</div>
      {/if}
      {#if state.input}
        <!-- svelte-ignore a11y-autofocus -->
        <input
          class="input modal-input mono"
          type="password"
          bind:value={inputValue}
          bind:this={inputEl}
          placeholder={state.placeholder || '管理员口令'}
          autocomplete="off"
          on:keydown={(e) => e.key === 'Enter' && answerConfirm(inputValue)}
        />
      {/if}
      <div class="modal-actions">
        <button class="btn btn-ghost" on:click={() => answerConfirm(false)}>
          {state.cancelText}
        </button>
        <button
          class="btn {state.danger ? 'btn-danger' : 'btn-primary'}"
          bind:this={confirmBtn}
          on:click={() => answerConfirm(state.input ? inputValue : true)}
        >
          {state.confirmText}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-title.danger {
    color: var(--danger);
  }
  .modal-input {
    width: 100%;
    margin: 0;
  }
  /* 输入模式：输入框与按钮之间留足间距，避免拥挤 */
  .modal-actions {
    margin-top: var(--s4);
  }
</style>

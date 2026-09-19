<script>
  // 全局确认弹窗（由 lib/confirm.js 的 confirmDialog() 驱动）
  import { confirmState, answerConfirm } from '../lib/confirm.js';
  import Icon from './Icon.svelte';

  let confirmBtn = null;

  $: state = $confirmState;
  $: if (state && confirmBtn) confirmBtn.focus();

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
      <div class="modal-actions">
        <button class="btn btn-ghost" on:click={() => answerConfirm(false)}>
          {state.cancelText}
        </button>
        <button
          class="btn {state.danger ? 'btn-danger' : 'btn-primary'}"
          bind:this={confirmBtn}
          on:click={() => answerConfirm(true)}
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
</style>

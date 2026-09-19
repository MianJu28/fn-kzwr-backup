<script>
  import { ICONS } from '../lib/icons.js';

  /** 图标名（见 lib/icons.js） */
  export let name = 'circle';
  /** 像素尺寸 */
  export let size = 18;
  /** 线宽 */
  export let stroke = 1.75;
  /** 无障碍标签（提供时作为 img 语义） */
  export let label = '';

  $: inner = ICONS[name] || ICONS.circle;
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  role={label ? 'img' : undefined}
  aria-label={label || undefined}
  aria-hidden={label ? undefined : 'true'}
>
  {@html inner}
</svg>

<style>
  .icon {
    display: block;
    flex-shrink: 0;
  }
</style>

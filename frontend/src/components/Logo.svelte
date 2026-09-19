<script>
  // 应用 Logo（蓝色圆角方块 + 白符号）。方案见 lib/logo.js 的 LOGO_VARIANT
  import { logoMarkup, LOGO_VARIANT } from '../lib/logo.js';

  export let variant = LOGO_VARIANT;
  export let size = 36;

  const uid = Math.random().toString(36).slice(2, 8);
  $: markup = logoMarkup(variant, `lg-${uid}-${variant}`);
</script>

<svg
  class="logo"
  width={size}
  height={size}
  viewBox="0 0 48 48"
  role="img"
  aria-label="酷族备份"
>
  {@html markup}
</svg>

<style>
  .logo {
    display: block;
    flex-shrink: 0;
    border-radius: 23%; /* 与 tile 的 rx=11/48 一致 */
    box-shadow: 0 2px 8px rgba(42, 92, 216, 0.28);
  }
</style>

<script>
  import Icon from './Icon.svelte';
  let { icon = 'inbox', title, children, action } = $props();
</script>

<div class="empty">
  <div class="ico"><Icon name={icon} size={22} /></div>
  <strong>{title}</strong>
  {#if children}<p class="muted small">{@render children()}</p>{/if}
  {#if action}<div class="act">{@render action()}</div>{/if}
</div>

<style>
  .empty { display: flex; flex-direction: column; align-items: center; text-align: center; gap: 0.35rem; padding: 3rem 1.5rem; }
  .ico { width: 2.75rem; height: 2.75rem; display: grid; place-items: center; border-radius: 12px; background: var(--surface-2); border: 1px solid var(--border); color: var(--text-2); margin-bottom: 0.5rem; }
  strong { font-size: 0.9375rem; }
  p { max-width: 44ch; line-height: 1.5; }
  .act { margin-top: 0.85rem; }
</style>

<script>
  import Icon from './Icon.svelte';

  /**
   * variant : primary | secondary | outline | ghost | danger
   * size    : sm | md | icon
   */
  let {
    variant = 'secondary',
    size = 'md',
    icon = '',
    label = '',
    loading = false,
    disabled = false,
    type = 'button',
    class: cls = '',
    children,
    ...rest
  } = $props();
</script>

<button class="btn {variant} {size} {cls}" {type} disabled={disabled || loading} aria-label={label || undefined} title={size === 'icon' ? label : undefined} {...rest}>
  {#if loading}<span class="spin" aria-hidden="true"></span>{:else if icon}<Icon name={icon} size={size === 'sm' ? 14 : 16} />{/if}
  {#if children}<span class="txt">{@render children()}</span>{/if}
</button>

<style>
  .btn {
    appearance: none;
    display: inline-flex; align-items: center; justify-content: center; gap: 0.45rem;
    height: 2.125rem; padding: 0 0.85rem;
    border-radius: var(--radius-sm);
    border: 1px solid transparent;
    font-size: 0.8125rem; font-weight: 550; line-height: 1;
    white-space: nowrap; cursor: pointer; user-select: none;
    transition: background 0.12s, border-color 0.12s, color 0.12s, box-shadow 0.12s;
  }
  .btn:active:not(:disabled) { transform: translateY(0.5px); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }

  .primary { background: var(--primary); color: var(--primary-fg); }
  .primary:hover:not(:disabled) { background: var(--primary-hover); }

  .secondary { background: var(--surface-2); color: var(--text); border-color: var(--border); }
  .secondary:hover:not(:disabled) { background: var(--surface-3); }

  .outline { background: var(--surface); color: var(--text); border-color: var(--border-strong); box-shadow: var(--shadow-sm); }
  .outline:hover:not(:disabled) { background: var(--surface-2); }

  .ghost { background: transparent; color: var(--text-2); }
  .ghost:hover:not(:disabled) { background: var(--surface-2); color: var(--text); }

  .danger { background: transparent; color: var(--bad); border-color: color-mix(in srgb, var(--bad) 35%, var(--border)); }
  .danger:hover:not(:disabled) { background: var(--bad-bg); }
  .danger.solid, .danger.primary { background: var(--bad); color: #fff; }

  .sm { height: 1.75rem; padding: 0 0.6rem; font-size: 0.75rem; gap: 0.35rem; }
  .icon { width: 2.125rem; padding: 0; }
  .icon.sm { width: 1.75rem; }

  .spin {
    width: 0.9rem; height: 0.9rem; border-radius: 50%;
    border: 2px solid currentColor; border-right-color: transparent;
    animation: r 0.7s linear infinite;
  }
  @keyframes r { to { transform: rotate(360deg); } }
</style>

<script>
  /** tone : neutral | ok | warn | bad | info ; dot : pastille de couleur ; mono : police à chasse fixe */
  let { tone = 'neutral', dot = false, mono = false, class: cls = '', title = undefined, children } = $props();
</script>

<span class="badge {tone} {cls}" class:mono {title}>
  {#if dot}<i aria-hidden="true"></i>{/if}
  {@render children?.()}
</span>

<style>
  .badge {
    display: inline-flex; align-items: center; gap: 0.35rem;
    height: 1.35rem; padding: 0 0.5rem;
    border-radius: 999px; font-size: 0.6875rem; font-weight: 600; line-height: 1; white-space: nowrap;
    background: var(--surface-2); color: var(--text-2);
    border: 1px solid transparent;
  }
  .badge.mono { font-weight: 500; border-radius: 6px; }
  i { width: 0.4rem; height: 0.4rem; border-radius: 50%; background: currentColor; }
  .ok { background: var(--ok-bg); color: var(--ok); }
  .warn { background: var(--warn-bg); color: var(--warn); }
  .bad { background: var(--bad-bg); color: var(--bad); }
  .info { background: var(--info-bg); color: var(--info); }
  .neutral { border-color: var(--border); }
</style>

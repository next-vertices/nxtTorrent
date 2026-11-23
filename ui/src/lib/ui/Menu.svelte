<script>
  import { tr } from '../i18n.js';
  import Icon from './Icon.svelte';

  /** items : [{ label, icon?, onclick, danger?, disabled? } | { separator: true }] */
  let { items = [], label = tr('Actions') } = $props();
  let open = $state(false);
  let btn = $state();
  let pos = $state({ top: 0, left: 0 });

  function toggle(e) {
    e.stopPropagation();
    if (open) {
      open = false;
      return;
    }
    const r = btn.getBoundingClientRect();
    // Ancré sous le bouton, aligné à droite ; remonte si le bas de l'écran est trop proche.
    const h = items.length * 32 + 12;
    pos = { top: r.bottom + h > innerHeight ? Math.max(8, r.top - h) : r.bottom + 4, left: Math.max(8, r.right - 192) };
    open = true;
  }

  $effect(() => {
    if (!open) return;
    const close = () => (open = false);
    const key = (e) => e.key === 'Escape' && close();
    window.addEventListener('click', close);
    window.addEventListener('keydown', key);
    window.addEventListener('scroll', close, true);
    window.addEventListener('resize', close);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('keydown', key);
      window.removeEventListener('scroll', close, true);
      window.removeEventListener('resize', close);
    };
  });
</script>

<button bind:this={btn} type="button" class="trigger" aria-label={label} aria-haspopup="menu" aria-expanded={open} onclick={toggle}>
  <Icon name="more" size={16} stroke={2.5} />
</button>

{#if open}
  <div class="menu" role="menu" style:top="{pos.top}px" style:left="{pos.left}px">
    {#each items as it, i (i)}
      {#if it.separator}
        <hr />
      {:else}
        <button type="button" role="menuitem" class:danger={it.danger} disabled={it.disabled} onclick={() => { open = false; it.onclick?.(); }}>
          {#if it.icon}<Icon name={it.icon} size={14} />{/if}{it.label}
        </button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .trigger {
    appearance: none; border: 0; background: transparent; color: var(--text-3);
    width: 1.75rem; height: 1.75rem; display: grid; place-items: center; border-radius: 6px; cursor: pointer;
  }
  .trigger:hover, .trigger[aria-expanded='true'] { background: var(--surface-3); color: var(--text); }
  .menu {
    position: fixed; z-index: 100; width: 192px; padding: 4px;
    background: var(--surface); border: 1px solid var(--border-strong); border-radius: 9px; box-shadow: var(--shadow-lg);
    animation: pop 0.1s ease-out;
  }
  .menu button {
    appearance: none; width: 100%; border: 0; background: transparent; color: var(--text);
    display: flex; align-items: center; gap: 0.55rem; height: 2rem; padding: 0 0.6rem; border-radius: 6px;
    font-size: 0.8125rem; cursor: pointer; text-align: left;
  }
  .menu button:hover:not(:disabled) { background: var(--surface-2); }
  .menu button:disabled { opacity: 0.45; cursor: not-allowed; }
  .menu button.danger { color: var(--bad); }
  .menu button.danger:hover:not(:disabled) { background: var(--bad-bg); }
  hr { border: 0; border-top: 1px solid var(--border); margin: 4px 2px; }
  @keyframes pop { from { opacity: 0; transform: translateY(-3px); } }
</style>

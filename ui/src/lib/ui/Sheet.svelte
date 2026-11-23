<script>
  import { tr } from '../i18n.js';
  import Icon from './Icon.svelte';

  /** Panneau latéral (détail d'une ligne de tableau). Natif <dialog> : Échap et focus gérés par le navigateur. */
  let { title, subtitle = '', onclose, children, footer } = $props();
  let dlg = $state();

  $effect(() => {
    if (dlg && !dlg.open) dlg.showModal();
  });

  function backdrop(e) {
    if (e.target === dlg) dlg.close();
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
<dialog bind:this={dlg} {onclose} onclick={backdrop} aria-label={title}>
  <div class="panel">
    <header>
      <div class="grow">
        <h3 class="truncate">{title}</h3>
        {#if subtitle}<p class="muted xs mono truncate">{subtitle}</p>{/if}
      </div>
      <button type="button" class="x" aria-label={tr('Fermer')} onclick={() => dlg.close()}><Icon name="x" size={16} /></button>
    </header>
    <div class="body">{@render children?.()}</div>
    {#if footer}<footer>{@render footer()}</footer>{/if}
  </div>
</dialog>

<style>
  dialog {
    margin: 0 0 0 auto; padding: 0; height: 100dvh; max-height: 100dvh; width: min(480px, 100vw); max-width: 100vw;
    border: 0; border-left: 1px solid var(--border); background: var(--surface); color: var(--text);
    box-shadow: var(--shadow-lg); animation: slide 0.18s ease-out;
  }
  dialog::backdrop { background: rgb(8 8 12 / 40%); animation: fade 0.18s ease-out; }
  .panel { display: flex; flex-direction: column; height: 100%; }
  header { display: flex; gap: 0.75rem; align-items: flex-start; padding: 1.1rem 1.25rem 0.9rem; border-bottom: 1px solid var(--border); }
  .x {
    appearance: none; border: 0; background: transparent; color: var(--text-3);
    width: 1.75rem; height: 1.75rem; display: grid; place-items: center; border-radius: 6px; cursor: pointer; flex: none;
  }
  .x:hover { background: var(--surface-2); color: var(--text); }
  .body { padding: 1.1rem 1.25rem; overflow: auto; flex: 1; display: flex; flex-direction: column; gap: 1.1rem; }
  footer { display: flex; gap: 0.5rem; padding: 0.8rem 1.25rem; border-top: 1px solid var(--border); background: var(--surface-2); }
  @keyframes slide { from { transform: translateX(24px); opacity: 0; } }
  @keyframes fade { from { opacity: 0; } }
</style>

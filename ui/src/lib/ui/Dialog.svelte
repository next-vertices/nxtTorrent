<script>
  import Icon from './Icon.svelte';

  /**
   * Boîte de dialogue (élément <dialog> natif : focus piégé, Échap pour fermer, rendu au-dessus de tout).
   * `size` : sm | md | lg.  Un clic sur le fond ferme la boîte.
   */
  let { title, description = '', size = 'md', onclose, children, footer } = $props();
  let dlg = $state();

  $effect(() => {
    if (dlg && !dlg.open) dlg.showModal();
  });

  function backdrop(e) {
    if (e.target === dlg) dlg.close();
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
<dialog bind:this={dlg} class={size} {onclose} onclick={backdrop} aria-labelledby="dlg-title">
  <div class="panel">
    <header>
      <div class="grow">
        <h3 id="dlg-title">{title}</h3>
        {#if description}<p class="muted small">{description}</p>{/if}
      </div>
      <button type="button" class="x" aria-label="Fermer" onclick={() => dlg.close()}><Icon name="x" size={16} /></button>
    </header>
    <div class="body">{@render children?.()}</div>
    {#if footer}<footer>{@render footer()}</footer>{/if}
  </div>
</dialog>

<style>
  dialog {
    padding: 0; border: 1px solid var(--border); border-radius: 14px;
    background: var(--surface); color: var(--text);
    box-shadow: var(--shadow-lg);
    max-height: calc(100dvh - 2rem); width: calc(100vw - 2rem);
    animation: pop 0.14s ease-out;
  }
  dialog.sm { max-width: 400px; }
  dialog.md { max-width: 540px; }
  dialog.lg { max-width: 720px; }
  dialog::backdrop { background: rgb(8 8 12 / 55%); backdrop-filter: blur(3px); animation: fade 0.14s ease-out; }
  .panel { display: flex; flex-direction: column; max-height: calc(100dvh - 2rem); }
  header { display: flex; gap: 0.75rem; align-items: flex-start; padding: 1.1rem 1.25rem 0.4rem; }
  .x {
    appearance: none; border: 0; background: transparent; color: var(--text-3);
    width: 1.75rem; height: 1.75rem; display: grid; place-items: center; border-radius: 6px; cursor: pointer; margin: -0.25rem -0.4rem 0 0;
  }
  .x:hover { background: var(--surface-2); color: var(--text); }
  .body { padding: 0.6rem 1.25rem 1.1rem; overflow: auto; display: flex; flex-direction: column; gap: 0.9rem; }
  footer {
    display: flex; justify-content: flex-end; gap: 0.5rem; padding: 0.8rem 1.25rem;
    border-top: 1px solid var(--border); background: var(--surface-2); border-radius: 0 0 14px 14px;
  }
  @keyframes pop { from { opacity: 0; transform: translateY(6px) scale(0.985); } }
  @keyframes fade { from { opacity: 0; } }
</style>

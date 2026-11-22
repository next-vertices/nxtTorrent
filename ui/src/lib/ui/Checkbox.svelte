<script>
  import Icon from './Icon.svelte';

  /** `indeterminate` : sélection partielle (case d'en-tête d'un tableau). */
  let { checked = $bindable(false), indeterminate = false, disabled = false, label = '', onchange, children } = $props();
</script>

<label class="cb" class:disabled>
  <input type="checkbox" bind:checked {onchange} {disabled} aria-label={label || undefined} />
  <span class="box" class:on={checked || indeterminate} aria-hidden="true">
    {#if indeterminate && !checked}<Icon name="minus" size={12} stroke={3} />{:else if checked}<Icon name="check" size={12} stroke={3} />{/if}
  </span>
  {#if children}<span class="txt">{@render children()}</span>{/if}
</label>

<style>
  .cb { display: inline-flex; align-items: center; gap: 0.5rem; cursor: pointer; font-size: 0.8125rem; }
  input { position: absolute; opacity: 0; width: 1rem; height: 1rem; margin: 0; cursor: pointer; }
  .box {
    width: 1rem; height: 1rem; border-radius: 4px; flex: none;
    border: 1px solid var(--border-strong); background: var(--surface);
    display: grid; place-items: center; color: var(--primary-fg);
    transition: background 0.1s, border-color 0.1s;
  }
  .box.on { background: var(--primary); border-color: var(--primary); }
  input:focus-visible + .box { outline: 2px solid var(--ring); outline-offset: 2px; }
  .cb:hover .box:not(.on) { border-color: var(--ring); }
  .disabled { opacity: 0.4; cursor: not-allowed; }
  .disabled input { cursor: not-allowed; }
</style>

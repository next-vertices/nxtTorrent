<script>
  import Icon from './Icon.svelte';

  /** options : [{ value, label }] ; `label` = nom accessible. */
  let { value = $bindable(''), options = [], label = '', class: cls = '', onchange, ...rest } = $props();
</script>

<div class="sel {cls}">
  <select bind:value aria-label={label} {onchange} {...rest}>
    {#each options as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
  </select>
  <Icon name="chevron-down" size={14} class="caret" />
</div>

<style>
  .sel { position: relative; display: inline-flex; align-items: center; }
  select {
    appearance: none; height: 2.125rem; padding: 0 1.9rem 0 0.7rem; min-width: 0; width: 100%;
    background: var(--surface); color: var(--text);
    border: 1px solid var(--border-strong); border-radius: var(--radius-sm);
    font: inherit; font-size: 0.8125rem; cursor: pointer; box-shadow: var(--shadow-sm);
  }
  select:hover { border-color: var(--ring); }
  select:focus { outline: none; border-color: var(--ring); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ring) 22%, transparent); }
  .sel :global(.caret) { position: absolute; right: 0.6rem; color: var(--text-3); pointer-events: none; }
  option { background: var(--surface); color: var(--text); }
</style>

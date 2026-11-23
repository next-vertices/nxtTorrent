<script>
  import { tr } from '../i18n.js';
  import Icon from './Icon.svelte';

  let {
    value = $bindable(''),
    icon = '',
    clearable = false,
    mono = false,
    class: cls = '',
    el = $bindable(),
    ...rest
  } = $props();
</script>

<div class="wrap {cls}" class:has-icon={icon}>
  {#if icon}<Icon name={icon} size={15} class="lead" />{/if}
  <input bind:this={el} bind:value class:mono {...rest} />
  {#if clearable && value}
    <button type="button" class="clear" aria-label={tr('Effacer')} onclick={() => (value = '')}><Icon name="x" size={14} /></button>
  {/if}
</div>

<style>
  .wrap { position: relative; display: flex; align-items: center; width: 100%; }
  .wrap :global(.lead) { position: absolute; left: 0.65rem; color: var(--text-3); pointer-events: none; }
  input {
    width: 100%; height: 2.125rem; padding: 0 0.7rem;
    background: var(--surface); color: var(--text);
    border: 1px solid var(--border-strong); border-radius: var(--radius-sm);
    font: inherit; font-size: 0.8125rem;
    box-shadow: var(--shadow-sm);
    transition: border-color 0.12s, box-shadow 0.12s;
  }
  .has-icon input { padding-left: 2rem; }
  input::placeholder { color: var(--text-3); }
  input:hover { border-color: var(--ring); }
  input:focus { outline: none; border-color: var(--ring); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ring) 22%, transparent); }
  input:disabled { opacity: 0.55; cursor: not-allowed; }
  .mono { font-family: var(--mono); font-size: 0.78rem; }
  .clear {
    position: absolute; right: 0.3rem; width: 1.5rem; height: 1.5rem; display: grid; place-items: center;
    border: 0; background: transparent; color: var(--text-3); border-radius: 5px; cursor: pointer;
  }
  .clear:hover { background: var(--surface-2); color: var(--text); }
</style>

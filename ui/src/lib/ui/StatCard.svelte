<script>
  import Icon from './Icon.svelte';
  import Sparkline from './Sparkline.svelte';

  /** Carte d'indicateur : valeur principale, détail, mini-courbe d'historique. */
  let { label, value, detail = '', icon = '', trend = [], color = 'dl', tone = '' } = $props();
</script>

<div class="stat card">
  <div class="top">
    <span class="lab">{label}</span>
    {#if icon}<Icon name={icon} size={15} class="ico" />{/if}
  </div>
  <div class="val tnum {tone}">{value}</div>
  <div class="det xs faint truncate">{detail || ' '}</div>
  <div class="spark"><Sparkline data={trend} {color} height={32} /></div>
</div>

<style>
  .stat { padding: 0.9rem 1rem 0.7rem; display: flex; flex-direction: column; gap: 0.1rem; min-width: 0; }
  .top { display: flex; justify-content: space-between; align-items: center; }
  .lab { font-size: 0.75rem; font-weight: 550; color: var(--text-2); }
  .stat :global(.ico) { color: var(--text-3); }
  .val { font-size: 1.65rem; font-weight: 650; letter-spacing: -0.025em; line-height: 1.25; margin-top: 0.15rem; }
  .val.ok { color: var(--ok); }
  .val.bad { color: var(--bad); }
  .spark { margin-top: 0.45rem; }
</style>

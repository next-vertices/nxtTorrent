<script>
  import { toasts, dismiss } from '../toast.svelte.js';
  import Icon from './Icon.svelte';

  const ICON = { ok: 'check', bad: 'alert', info: 'info' };
</script>

<div class="stack-t" aria-live="polite">
  {#each toasts as t (t.id)}
    <div class="toast {t.kind}" role={t.kind === 'bad' ? 'alert' : 'status'}>
      <Icon name={ICON[t.kind]} size={16} />
      <span class="grow">{t.text}</span>
      <button type="button" aria-label="Fermer" onclick={() => dismiss(t.id)}><Icon name="x" size={14} /></button>
    </div>
  {/each}
</div>

<style>
  /* Bas et centre : à l'écart des barres d'outils (haut) et des boutons de pagination (bas droite). Sur mobile : en haut. */
  .stack-t { position: fixed; bottom: 1.25rem; left: 50%; transform: translateX(-50%); z-index: 200; pointer-events: none; display: flex; flex-direction: column; gap: 0.5rem; width: min(380px, calc(100vw - 2rem)); }
  .toast {
    pointer-events: auto;
    display: flex; gap: 0.6rem; align-items: center; padding: 0.65rem 0.8rem;
    background: var(--surface); border: 1px solid var(--border-strong); border-radius: 10px; box-shadow: var(--shadow-lg);
    font-size: 0.8125rem; animation: in 0.18s ease-out;
  }
  .toast.ok > :global(svg:first-child) { color: var(--ok); }
  .toast.bad { border-color: color-mix(in srgb, var(--bad) 40%, var(--border)); }
  .toast.bad > :global(svg:first-child) { color: var(--bad); }
  .toast.info > :global(svg:first-child) { color: var(--info); }
  button { appearance: none; border: 0; background: transparent; color: var(--text-3); cursor: pointer; display: grid; place-items: center; padding: 2px; border-radius: 4px; }
  button:hover { color: var(--text); background: var(--surface-2); }
  @keyframes in { from { opacity: 0; transform: translateY(8px); } }
  @media (max-width: 860px) {
    .stack-t { bottom: auto; top: 3.75rem; }
  }
</style>

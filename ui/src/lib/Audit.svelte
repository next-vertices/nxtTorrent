<script>
  import { onMount } from 'svelte';
  import { api, qs } from './api.js';
  import { live } from './live.svelte.js';
  import { ago, clock, timeOnly, dayLabel, CATEGORIES } from './format.js';
  import Segmented from './ui/Segmented.svelte';
  import Input from './ui/Input.svelte';
  import Icon from './ui/Icon.svelte';
  import Badge from './ui/Badge.svelte';
  import Button from './ui/Button.svelte';
  import Checkbox from './ui/Checkbox.svelte';
  import EmptyState from './ui/EmptyState.svelte';

  const CAT_ICON = { security: 'shield', access: 'lock', nodes: 'server', torrents: 'download', system: 'cpu' };
  const SEV = {
    info: { label: 'Information', tone: 'neutral' },
    warning: { label: 'Alerte', tone: 'warn' },
    critical: { label: 'Critique', tone: 'bad' },
  };
  const TABS = [{ value: '', label: 'Tout' }, ...Object.entries(CATEGORIES).map(([value, label]) => ({ value, label }))];

  let category = $state('');
  let alertsOnly = $state(false);
  let q = $state('');
  let qDebounced = $state('');
  let items = $state([]);
  let next = $state(null);
  let loading = $state(true);
  let more = $state(false);
  let error = $state('');
  let showRaw = $state(false);

  let timer;
  $effect(() => {
    q;
    clearTimeout(timer);
    timer = setTimeout(() => (qDebounced = q), 250);
    return () => clearTimeout(timer);
  });

  let seq = 0;
  async function load({ passive = false } = {}) {
    const mine = ++seq;
    try {
      const r = await api('GET', `/audit${qs({ limit: 50, category, severity: alertsOnly ? 'warning' : '', q: qDebounced })}`, undefined, { passive });
      if (mine !== seq) return;
      items = r.items;
      next = r.next_before;
      error = '';
    } catch (e) {
      if (mine === seq) error = e.message;
    } finally {
      if (mine === seq) loading = false;
    }
  }

  async function loadMore() {
    more = true;
    try {
      const r = await api('GET', `/audit${qs({ limit: 50, before: next, category, severity: alertsOnly ? 'warning' : '', q: qDebounced })}`);
      items = [...items, ...r.items];
      next = r.next_before;
    } catch (e) {
      error = e.message;
    } finally {
      more = false;
    }
  }

  $effect(() => {
    category; alertsOnly; qDebounced;
    load();
  });

  // Les nouveaux événements arrivent en direct tant que l'on regarde le haut de la liste.
  let last = 0;
  $effect(() => {
    live.rev;
    if (Date.now() - last > 3000 && items.length <= 50) {
      last = Date.now();
      load({ passive: true });
    }
  });
  onMount(() => () => clearTimeout(timer));

  // Regroupement par jour pour la lecture.
  const groups = $derived.by(() => {
    const out = [];
    for (const e of items) {
      const label = dayLabel(e.ts);
      const g = out[out.length - 1];
      if (g && g.label === label) g.items.push(e);
      else out.push({ label, items: [e] });
    }
    return out;
  });
</script>

<div class="stack-lg fill">
  <div class="head">
    <div>
      <h1>Journal</h1>
      <p class="muted">Tout ce qui s'est passé sur votre cluster : qui a fait quoi, et les événements de sécurité.</p>
    </div>
  </div>

  <section class="card panel">
    <div class="toolbar">
      <Segmented bind:value={category} options={TABS} label="Catégorie" />
      <div class="grow search"><Input bind:value={q} icon="search" clearable placeholder="Rechercher dans le journal…" aria-label="Rechercher dans le journal" /></div>
      <Checkbox bind:checked={alertsOnly}>Alertes seulement</Checkbox>
    </div>

    {#if error}<p class="callout bad m" role="alert"><Icon name="alert" size={16} /><span>{error}</span></p>{/if}

    {#if loading}
      <div class="sks">{#each Array(6) as _, i (i)}<div class="sk"></div>{/each}</div>
    {:else if items.length === 0}
      <EmptyState icon="shield" title="Aucun événement">Aucune entrée ne correspond à ces filtres.</EmptyState>
    {:else}
      <div class="timeline scrolly">
        {#each groups as g (g.label)}
          <h3 class="day">{g.label}</h3>
          <ul>
            {#each g.items as e (e.id)}
              <li class="ev {e.severity}">
                <span class="ic" title={SEV[e.severity].label}><Icon name={CAT_ICON[e.category]} size={15} /></span>
                <div class="body">
                  <p class="title">{e.title}</p>
                  <p class="meta xs">
                    <span class="who {e.actor_kind}"><Icon name={e.actor_kind === 'node' ? 'server' : e.actor_kind === 'admin' ? 'user' : e.actor_kind === 'anonymous' ? 'alert' : 'cpu'} size={11} />{e.actor_label}</span>
                    {#if e.detail}<span class="det">{e.detail}</span>{/if}
                    {#if showRaw}<code class="mono raw">{e.action}</code>{/if}
                  </p>
                </div>
                {#if e.severity !== 'info'}<Badge tone={SEV[e.severity].tone}>{SEV[e.severity].label}</Badge>{/if}
                <time class="xs faint nowrap tnum" datetime={new Date(e.ts * 1000).toISOString()} title="{clock(e.ts)} ({ago(e.ts)})">{timeOnly(e.ts)}</time>
              </li>
            {/each}
          </ul>
        {/each}
      </div>

      <footer class="foot">
        {#if next}<Button variant="outline" loading={more} onclick={loadMore}>Charger plus</Button>{:else}<span class="xs faint">Fin du journal</span>{/if}
        <span class="grow"></span>
        <Checkbox bind:checked={showRaw}>Codes techniques</Checkbox>
      </footer>
    {/if}
  </section>
</div>

<style>
  /* La page tient dans la fenêtre : seule la liste défile (haut de page 1,5 rem + bas 4 rem = 5,5 rem de marges). */
  .fill { height: calc(100dvh - 5.5rem); min-height: 24rem; }
  .panel { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .panel > .toolbar, .panel > .foot, .panel > .callout { flex: none; }
  .scrolly { flex: 1; min-height: 0; overflow-y: auto; overscroll-behavior: contain; }
  /* Petits écrans : la page redevient normale (barre du haut, pas de hauteur fixe). */
  @media (max-width: 860px) {
    .fill { height: auto; min-height: 0; }
    .scrolly { overflow: visible; }
  }
  .head { display: flex; justify-content: space-between; align-items: flex-end; gap: 1rem; flex-wrap: wrap; }
  .toolbar { display: flex; gap: 0.75rem; padding: 0.85rem 1rem; flex-wrap: wrap; align-items: center; border-bottom: 1px solid var(--border); }
  .search { min-width: 14rem; }
  .m { margin: 0.75rem 1rem; }
  .timeline { padding: 0.25rem 0 0.5rem; }
  .day { font-size: 0.6875rem; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-3); padding: 1rem 1.1rem 0.4rem; font-weight: 600; }
  ul { list-style: none; margin: 0; padding: 0 0.5rem; }
  .ev { display: flex; align-items: center; gap: 0.8rem; padding: 0.55rem 0.6rem; border-radius: 9px; }
  .ev:hover { background: var(--surface-2); }
  .ic { width: 1.9rem; height: 1.9rem; border-radius: 50%; display: grid; place-items: center; flex: none; background: var(--surface-2); color: var(--text-2); border: 1px solid var(--border); }
  .warning .ic { background: var(--warn-bg); color: var(--warn); border-color: transparent; }
  .critical .ic { background: var(--bad-bg); color: var(--bad); border-color: transparent; }
  .body { flex: 1; min-width: 0; }
  .title { font-size: 0.8125rem; line-height: 1.4; font-weight: 520; }
  .meta { display: flex; flex-wrap: wrap; align-items: center; gap: 0.2rem 0.6rem; color: var(--text-3); margin-top: 0.1rem; }
  .who { display: inline-flex; align-items: center; gap: 0.3rem; color: var(--text-2); font-weight: 550; }
  .who.anonymous { color: var(--warn); }
  .raw { background: var(--surface-3); padding: 0 0.3rem; border-radius: 4px; font-size: 0.6875rem; }
  .foot { display: flex; align-items: center; gap: 0.8rem; padding: 0.7rem 1rem; border-top: 1px solid var(--border); }
  .sks { padding: 1rem; display: flex; flex-direction: column; gap: 0.6rem; }
  .sk { height: 2.4rem; border-radius: 8px; background: linear-gradient(100deg, var(--surface-2) 30%, var(--surface-3) 50%, var(--surface-2) 70%); background-size: 200% 100%; animation: sh 1.4s linear infinite; }
  @keyframes sh { to { background-position: -200% 0; } }
</style>

<script>
  import { ago, bytes, rate, num } from './format.js';
  import Badge from './ui/Badge.svelte';
  import Icon from './ui/Icon.svelte';

  /** Corps du détail d'un nœud : partagé par la vue en cartes et par le panneau latéral. */
  let { n, stat = null, onlabels, onlocate, onlimits } = $props();

  const PORT = {
    open: { label: 'Port joignable', tone: 'ok' },
    closed: { label: 'Port fermé ?', tone: 'bad' },
    unknown: { label: 'Joignabilité inconnue', tone: 'neutral' },
  };
  const port = $derived(PORT[n.network.incoming] ?? PORT.unknown);
  const issues = $derived(n.network.findings.filter((f) => f.tone === 'bad' || f.tone === 'warn'));
  const pct0 = (x) => `${Math.round(x * 100)} %`;
  const lim = (b) => (b ? `${b >= 1e6 ? (b / 1e6).toFixed(b % 1e6 ? 1 : 0).replace('.', ',') + ' Mo/s' : Math.round(b / 1000) + ' Ko/s'}` : 'illimité');
</script>

{#if n.state !== 'Revoked'}
  <div class="row wrap labels">
    {#each n.labels as l (l)}<Badge mono>{l}</Badge>{/each}
    <button type="button" class="addl" onclick={onlabels} title="Modifier les labels de ce nœud">
      <Icon name={n.labels.length ? 'edit' : 'plus'} size={11} stroke={2.5} />{n.labels.length ? 'Modifier' : 'Ajouter des labels'}
    </button>
  </div>
  <p class="where xs">
    <Icon name="wifi" size={12} />
    {#if n.location}<strong>{n.location}</strong>{:else}<button type="button" class="addl" onclick={onlocate}><Icon name="plus" size={11} stroke={2.5} />Ajouter une localisation</button>{/if}
    {#if n.last_ip}<span class="faint mono" title="Dernière adresse source vue par le master (peut être celle d'un NAT, d'un proxy ou du réseau Docker)">{n.last_ip}</span>{/if}
  </p>
{:else if n.labels.length}
  <div class="row wrap">{#each n.labels as l (l)}<Badge mono>{l}</Badge>{/each}</div>
{/if}

<dl>
  <div><dt>Client</dt><dd>{n.client_kind && n.client_kind !== 'none' ? `${n.client_kind} ${n.client_version}` : '—'}</dd></div>
  <div><dt>Torrents</dt><dd class="tnum">{num(n.torrent_count)}</dd></div>
  <div><dt>Espace libre</dt><dd class="tnum">{n.disk_free ? bytes(n.disk_free) : '—'}</dd></div>
  <div><dt>Débit</dt><dd class="tnum nowrap"><span class="d">↓ {rate(n.dl_rate)}</span> <span class="u">↑ {rate(n.ul_rate)}</span></dd></div>
  <div><dt>Dernière activité</dt><dd>{n.connected ? 'connecté' : ago(n.last_seen)}</dd></div>
  <div><dt>Synchronisation</dt><dd>{#if n.applied_rev === n.desired_rev}à jour{:else}rév. {n.applied_rev} / {n.desired_rev}{/if}</dd></div>
  <div class="wide">
    <dt>Limites de débit</dt>
    <dd>
      {#if n.limits_managed}<span class="d">↓ {lim(n.dl_limit)}</span> <span class="u">↑ {lim(n.ul_limit)}</span>{:else}<span class="faint">réglages du client (non gérés ici)</span>{/if}
      {#if n.state !== 'Revoked'}<button type="button" class="addl inl" onclick={onlimits}><Icon name="edit" size={11} stroke={2.5} />Modifier</button>{/if}
    </dd>
  </div>
  {#if stat}
    <div><dt>Envoyé (période)</dt><dd class="tnum">{bytes(stat.up_bytes)} · {pct0(stat.up_share)}</dd></div>
    <div><dt>Disponibilité</dt><dd class="tnum" class:warnc={stat.availability < 0.95}>{pct0(stat.availability)}</dd></div>
  {/if}
</dl>

{#if stat?.underperforming}
  <p class="callout warn"><Icon name="alert" size={15} /><span><strong>Envoie bien moins que les autres nœuds</strong> sur la période (moins du quart de la médiane du cluster). Vérifiez le port, les limites d'envoi et le nombre de torrents réellement actifs.</span></p>
{/if}

{#if n.state === 'Online'}
  <section class="net" aria-label="Santé réseau">
    <div class="row wrap between">
      <Badge tone={port.tone} dot>{port.label}</Badge>
      <span class="xs faint tnum">
        {num(n.network.peers)} pairs{n.network.listen_port ? ` · port ${n.network.listen_port}` : ''}{n.network.dht_nodes ? ` · DHT ${num(n.network.dht_nodes)}` : ''}
      </span>
    </div>
    {#each issues as f (f.title)}
      <div class="callout {f.tone}"><Icon name="alert" size={15} /><div class="stack tight"><strong>{f.title}</strong><span>{f.text}</span></div></div>
    {:else}
      {#each n.network.findings.slice(0, 1) as f (f.title)}<p class="xs faint">{f.text}</p>{/each}
    {/each}
  </section>
{/if}

<style>
  .where { display: flex; align-items: center; gap: 0.45rem; color: var(--text-2); flex-wrap: wrap; }
  dl { margin: 0; display: grid; grid-template-columns: 1fr 1fr; gap: 0.7rem 1rem; padding-top: 0.85rem; border-top: 1px solid var(--border); }
  dl div { min-width: 0; }
  dt { font-size: 0.6875rem; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.05em; margin-bottom: 0.1rem; }
  dd { margin: 0; font-size: 0.8125rem; font-weight: 550; overflow: hidden; text-overflow: ellipsis; }
  .d { color: var(--chart-dl); } .u { color: var(--chart-ul); }
  .warnc { color: var(--warn); }
  .wide { grid-column: 1 / -1; }
  .inl { margin-left: 0.5rem; vertical-align: middle; }
  .labels { gap: 0.35rem; }
  .net { display: flex; flex-direction: column; gap: 0.6rem; padding-top: 0.85rem; border-top: 1px solid var(--border); }
  .tight { gap: 0.15rem; }
  .callout strong { font-weight: 650; }
  .addl { appearance: none; display: inline-flex; align-items: center; gap: 0.25rem; height: 1.35rem; padding: 0 0.5rem; border: 1px dashed var(--border-strong); border-radius: 999px; background: transparent; color: var(--text-3); font: inherit; font-size: 0.6875rem; cursor: pointer; }
  .addl:hover { color: var(--text); border-style: solid; background: var(--surface-2); }
</style>

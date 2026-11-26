<script>
  import Rich from './ui/Rich.svelte';
  import { tr } from './i18n.js';
  import { ago, bytes, rate, num, dec1 } from './format.js';
  import Badge from './ui/Badge.svelte';
  import Icon from './ui/Icon.svelte';

  /** Corps du détail d'un nœud : partagé par la vue en cartes et par le panneau latéral. */
  let { n, stat = null, onlabels, onlocate, onlimits } = $props();

  const PORT = {
    open: { label: tr('Port joignable'), tone: 'ok' },
    closed: { label: tr('Port fermé ?'), tone: 'bad' },
    unknown: { label: tr('Joignabilité inconnue'), tone: 'neutral' },
  };
  const port = $derived(PORT[n.network.incoming] ?? PORT.unknown);
  const issues = $derived(n.network.findings.filter((f) => f.tone === 'bad' || f.tone === 'warn'));
  const pct0 = (x) => `${Math.round(x * 100)} %`;
  const lim = (b) => (b ? (b >= 1e6 ? tr('{v} Mo/s', { v: dec1(b / 1e6) }) : tr('{v} Ko/s', { v: Math.round(b / 1000) })) : tr('illimité'));
</script>

{#if n.state !== 'Revoked'}
  <div class="row wrap labels">
    {#each n.labels as l (l)}<Badge mono>{l}</Badge>{/each}
    <button type="button" class="addl" onclick={onlabels} title={tr('Modifier les labels de ce nœud')}>
      <Icon name={n.labels.length ? 'edit' : 'plus'} size={11} stroke={2.5} />{n.labels.length ? tr('Modifier') : tr('Ajouter des labels')}
    </button>
  </div>
  {#if n.update?.updating}
    <p class="callout small" role="status"><Icon name="refresh" size={15} /><span>{tr('Mise à jour de l\'agent en cours…')}</span></p>
  {:else if n.update?.last && ['failed', 'rolled_back', 'rejected'].includes(n.update.last.state)}
    <p class="callout warn small"><Icon name="alert" size={15} /><span>{n.update.last.state === 'rolled_back' ? tr('La version {version} n\'a pas fonctionné : la précédente a été rétablie.', { version: n.update.last.version }) : tr('La mise à jour vers {version} a échoué.', { version: n.update.last.version })} <span class="faint">{n.update.last.reason}</span></span></p>
  {:else if n.update?.available}
    <p class="xs faint">{n.update.capable ? tr('Agent {current} : version {version} disponible (menu du nœud).', { current: n.agent_version, version: n.update.available }) : tr('Agent {current} : version {version} disponible, à installer à la main sur ce nœud.', { current: n.agent_version, version: n.update.available })}</p>
  {/if}
  <p class="where xs">
    <Icon name="wifi" size={12} />
    {#if n.location}<strong>{n.location}</strong>{:else}<button type="button" class="addl" onclick={onlocate}><Icon name="plus" size={11} stroke={2.5} />{tr('Ajouter une localisation')}</button>{/if}
    {#if n.last_ip}<span class="faint mono" title={tr("Dernière adresse source vue par le master (peut être celle d'un NAT, d'un proxy ou du réseau Docker)")}>{n.last_ip}</span>{/if}
  </p>
{:else if n.labels.length}
  <div class="row wrap">{#each n.labels as l (l)}<Badge mono>{l}</Badge>{/each}</div>
{/if}

<dl>
  <div><dt>{tr('Agent')}</dt><dd class="tnum">{n.agent_version}
    <span class="xs faint">{n.update?.available ? tr('mise à jour disponible') : tr('à jour')}{#if n.connected} · {n.update?.capable ? tr('mise à jour automatique') : tr('mise à jour manuelle')}{/if}</span></dd></div>
  <div><dt>{tr('Client')}</dt><dd>{n.client_kind && n.client_kind !== 'none' ? `${n.client_kind} ${n.client_version}` : '—'}</dd></div>
  <div><dt>{tr('Torrents')}</dt><dd class="tnum">{num(n.torrent_count)}</dd></div>
  <div><dt>{tr('Espace libre')}</dt><dd class="tnum">{n.disk_free ? bytes(n.disk_free) : '—'}</dd></div>
  <div><dt>{tr('Débit')}</dt><dd class="tnum nowrap"><span class="d">↓ {rate(n.dl_rate)}</span> <span class="u">↑ {rate(n.ul_rate)}</span></dd></div>
  <div><dt>{tr('Dernière activité')}</dt><dd>{n.connected ? tr('connecté') : ago(n.last_seen)}</dd></div>
  <div><dt>{tr('Synchronisation')}</dt><dd>{#if n.applied_rev === n.desired_rev}{tr('à jour')}{:else}{tr('rév. {applied_rev} / {desired_rev}', { applied_rev: n.applied_rev, desired_rev: n.desired_rev })}{/if}</dd></div>
  <div class="wide">
    <dt>{tr('Limites de débit')}</dt>
    <dd>
      {#if n.limits_managed}<span class="d">↓ {lim(n.dl_limit)}</span> <span class="u">↑ {lim(n.ul_limit)}</span>{:else}<span class="faint">{tr('réglages du client (non gérés ici)')}</span>{/if}
      {#if n.state !== 'Revoked'}<button type="button" class="addl inl" onclick={onlimits}><Icon name="edit" size={11} stroke={2.5} />{tr('Modifier')}</button>{/if}
    </dd>
  </div>
  {#if stat}
    <div><dt>{tr('Envoyé (période)')}</dt><dd class="tnum">{bytes(stat.up_bytes)} · {pct0(stat.up_share)}</dd></div>
    <div><dt>{tr('Disponibilité')}</dt><dd class="tnum" class:warnc={stat.availability < 0.95}>{pct0(stat.availability)}</dd></div>
  {/if}
</dl>

{#if stat?.underperforming}
  <p class="callout warn"><Icon name="alert" size={15} /><span><Rich text={tr('**Envoie bien moins que les autres nœuds** sur la période (moins du quart de la médiane du cluster). Vérifiez le port, les limites d\'envoi et le nombre de torrents réellement actifs.')} /></span></p>
{/if}

{#if n.state === 'Online'}
  <section class="net" aria-label={tr('Santé réseau')}>
    <div class="row wrap between">
      <Badge tone={port.tone} dot>{port.label}</Badge>
      <span class="xs faint tnum">
        {tr('{n} pair|{n} pairs', { n: n.network.peers })}{n.network.listen_port ? ` · ${tr('port {port}', { port: n.network.listen_port })}` : ''}{n.network.dht_nodes ? ` · DHT ${num(n.network.dht_nodes)}` : ''}
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
  .callout :global(strong) { font-weight: 650; }
  .addl { appearance: none; display: inline-flex; align-items: center; gap: 0.25rem; height: 1.35rem; padding: 0 0.5rem; border: 1px dashed var(--border-strong); border-radius: 999px; background: transparent; color: var(--text-3); font: inherit; font-size: 0.6875rem; cursor: pointer; }
  .addl:hover { color: var(--text); border-style: solid; background: var(--surface-2); }
</style>

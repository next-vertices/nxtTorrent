<script>
  import { bytes, rate, pct, num, ratioTxt, dateOnly, STATUS, diagnose } from './format.js';
  import { copyText } from './clipboard.js';
  import { toast } from './toast.svelte.js';
  import Sheet from './ui/Sheet.svelte';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Progress from './ui/Progress.svelte';
  import Icon from './ui/Icon.svelte';

  let { hash, items, onclose, onpause, onresume, onremove, onplacement, onrecheck } = $props();

  // La ligne vient de la page affichée et se met à jour en direct ; on garde le dernier état connu si elle disparaît
  // (filtre modifié, torrent supprimé) pour ne pas vider le panneau sous les yeux de l'utilisateur.
  let snap = $state(null);
  $effect(() => {
    const t = items.find((x) => x.infohash === hash);
    if (t) snap = t;
  });
  const t = $derived(snap);
  const notes = $derived(t ? diagnose(t) : []);

  async function copy(text) {
    try {
      if (!(await copyText(text))) throw new Error('copie refusée');
      toast.success('Copié dans le presse-papiers.');
    } catch {
      toast.error('Copie impossible dans ce navigateur.');
    }
  }
  const ROLE = { primary: 'Principal', replica: 'Copie' };
  const REPL = {
    swarm: 'copie entre nœuds par BitTorrent',
    waiting_file: 'attend le fichier .torrent (demandé à un nœud)',
    shared: 'fichiers supposés déjà présents sur les nœuds cibles',
  };
  const PHASE = {
    Transfer: 'copie depuis les autres nœuds (BitTorrent)',
    Retarget: 'retour au torrent d’origine',
    Adding: 'ajout en pause',
    Rechecking: 'vérification des données',
  };
  const ICON = { bad: 'alert', warn: 'alert', info: 'info' };
  const PROG_TONE = { seeding: 'ok', downloading: 'info', checking: 'warn', error: 'bad' };
</script>

{#if t}
  {@const st = STATUS[t.status] ?? { label: t.status, tone: 'neutral' }}
  <Sheet title={t.name} subtitle={t.managed ? 'Géré par nxtTorrent' : 'Présent dans le client, non géré'} {onclose}>
    <div class="row wrap">
      <Badge tone={st.tone} dot>{st.label}</Badge>
      {#if !t.managed}<Badge>Non géré</Badge>{/if}
    </div>

    {#if notes.length}
      <div class="stack" aria-label="Diagnostic">
        {#each notes as n (n.title)}
          <div class="callout {n.tone}">
            <Icon name={ICON[n.tone]} size={16} />
            <div class="stack tight">
              <strong>{n.title}</strong>
              <span>{n.text}</span>
            </div>
          </div>
        {/each}
      </div>
    {/if}

    {#if t.nodes.length}
      <div class="stack">
        <div class="row between">
          <span class="small muted">{t.size ? `${bytes(t.size * t.progress)} sur ${bytes(t.size)}` : 'Progression'}</span>
          <strong class="tnum small">{pct(t.progress)}</strong>
        </div>
        <Progress value={t.progress} tone={PROG_TONE[t.status] ?? 'neutral'} height={8} label="Progression" />
      </div>
    {/if}

    <dl>
      <div><dt>Taille</dt><dd class="tnum">{t.size ? bytes(t.size) : '—'}</dd></div>
      <div><dt>Débit{t.nodes.length > 1 ? ' (tous nœuds)' : ''}</dt><dd class="tnum"><span class="d">↓ {rate(t.dl_rate)}</span> <span class="u">↑ {rate(t.ul_rate)}</span></dd></div>
      <div><dt>Envoyé / téléchargé</dt><dd class="tnum"><span class="u">↑ {t.uploaded ? bytes(t.uploaded) : '—'}</span> <span class="d">↓ {t.downloaded ? bytes(t.downloaded) : '—'}</span></dd></div>
      <div><dt>Ratio</dt><dd class="tnum">{ratioTxt(t.ratio)}</dd></div>
      <div><dt>Ajouté le</dt><dd>{t.added_on ? dateOnly(t.added_on) : 'inconnu'}</dd></div>
      <div><dt>Pairs connectés</dt><dd class="tnum">{t.peers_connected ? `${num(t.peers_connected)}${t.peers_active ? ` (${num(t.peers_active)} actifs)` : ''}` : '—'}</dd></div>
      <div><dt>Essaim (tracker)</dt><dd class="tnum">{#if t.swarm_seeds || t.swarm_leechers}<span class="u">{num(t.swarm_seeds)}</span> complets · <span class="d">{num(t.swarm_leechers)}</span> incomplets{:else}<span class="faint">inconnu</span>{/if}</dd></div>
      <div class="wide"><dt>Tracker</dt><dd class="mono">{t.tracker || 'inconnu'}</dd></div>
      <div><dt>Dossier</dt><dd class="mono">{t.save_subdir || '/'}</dd></div>
      <div><dt>Réplication</dt><dd>{t.replicate_group ? `vers « ${t.replicate_group} »` : 'aucune'}{#if REPL[t.replication]}<span class="xs faint"> · {REPL[t.replication]}</span>{/if}</dd></div>
      <div class="wide"><dt>Labels requis{#if t.managed}<button type="button" class="edit" onclick={() => onplacement(t)}><Icon name="edit" size={11} stroke={2.5} /> Modifier</button>{/if}</dt><dd class="row wrap">{#each t.requires as l (l)}<Badge mono>{l}</Badge>{:else}<span class="faint">aucun : n'importe quel nœud convient</span>{/each}</dd></div>
    </dl>

    <section class="stack">
      <h3>Nœuds ({t.nodes.length})</h3>
      {#each t.nodes as n (n.node_id)}
        {@const ns = STATUS[n.status] ?? { label: n.status, tone: 'neutral' }}
        <div class="nrow" class:off={!n.node_online}>
          <div class="row between">
            <span class="row nm"><i class="pip" class:on={n.node_online} title={n.node_online ? 'Connecté' : 'Hors ligne'}></i><span class="truncate">{n.node}</span></span>
            <span class="row">
              <Badge>{ROLE[n.role] ?? n.role}</Badge>
              <Badge tone={ns.tone} dot>{ns.label}</Badge>
            </span>
          </div>
          <div class="pw"><Progress value={n.progress} tone={PROG_TONE[n.status] ?? 'neutral'} height={4} label="Progression sur {n.node}" /><span class="xs tnum">{pct(n.progress)}</span></div>
          {#if n.status !== 'waiting'}
            <span class="xs tnum nstats"><span class="d">↓ {rate(n.dl_rate)}</span> <span class="u">↑ {rate(n.ul_rate)}</span> · envoyé {n.uploaded ? bytes(n.uploaded) : '—'}{n.downloaded ? ` · reçu ${bytes(n.downloaded)}` : ''} · ratio {ratioTxt(n.ratio)}</span>
          {/if}
          {#if n.peers_connected}<span class="xs faint">{num(n.peers_connected)} pairs connectés{n.peers_active ? `, ${num(n.peers_active)} actifs` : ''}</span>{/if}
          {#if n.message}<p class="msg xs"><Icon name={n.status === 'error' ? 'alert' : 'info'} size={12} /> {n.message}</p>{/if}
          {#if !n.node_online}<span class="xs faint">Nœud hors ligne : dernier état connu.</span>{/if}
          {#if n.phase && n.phase !== 'Active'}<span class="xs faint">Réplication : {PHASE[n.phase] ?? n.phase}</span>{/if}
        </div>
      {:else}
        <p class="small muted">Aucun nœud ne porte ce torrent pour l'instant.</p>
      {/each}
    </section>

    {#if t.members?.length}
      <section class="stack">
        <h3>Même contenu sur {t.members.length} autre{t.members.length > 1 ? 's' : ''} tracker{t.members.length > 1 ? 's' : ''}</h3>
        <p class="xs faint">Ajoutés par Multi-trackers sur les mêmes fichiers : un seul stockage, plusieurs ratios. Les débits et les pairs ci-dessus les incluent.</p>
        {#each t.members as m (m.infohash)}
          {@const ms = STATUS[m.status] ?? { label: m.status, tone: 'neutral' }}
          <div class="nrow" class:off={!m.node_online}>
            <div class="row between">
              <span class="row nm"><Badge mono>{m.tracker || 'tracker inconnu'}</Badge>{#if m.linked}<Badge title="Liens physiques vers les fichiers de l'original : aucun espace disque en plus">liens</Badge>{/if}<span class="xs faint truncate">{m.node}</span></span>
              <Badge tone={ms.tone} dot>{ms.label}</Badge>
            </div>
            <div class="pw"><Progress value={m.progress} tone={PROG_TONE[m.status] ?? 'neutral'} height={4} label="Progression sur {m.tracker}" /><span class="xs tnum">{pct(m.progress)}</span></div>
            <span class="xs faint">↑ {rate(m.ul_rate)} · envoyé {bytes(m.uploaded)} · ratio {ratioTxt(m.ratio)}{m.peers_connected ? ` · ${num(m.peers_connected)} pairs` : ''}</span>
            {#if m.message}<p class="msg xs"><Icon name="alert" size={12} /> {m.message}</p>{/if}
          </div>
        {/each}
      </section>
    {/if}

    {#if !t.managed}
      <p class="callout"><Icon name="info" size={15} /><span>Ce torrent était déjà présent dans le client du nœud. nxtTorrent ne le modifie jamais : pour le piloter ici, supprimez-le du client puis ajoutez-le à nouveau depuis cette interface.</span></p>
    {/if}

    <details class="tech">
      <summary>Détails techniques</summary>
      <div class="row between"><code class="mono ih">{t.infohash}</code><Button variant="ghost" size="icon" icon="copy" label="Copier l'infohash" onclick={() => copy(t.infohash)} /></div>
    </details>

    {#snippet footer()}
      {#if t.managed}
        {#if t.paused}
          <Button variant="outline" icon="play" onclick={() => onresume(t.infohash)}>Reprendre</Button>
        {:else}
          <Button variant="outline" icon="pause" onclick={() => onpause(t.infohash)}>Mettre en pause</Button>
        {/if}
        <Button variant="outline" icon="refresh" onclick={() => onrecheck(t.infohash)}>Revérifier</Button>
        <span class="grow"></span>
        <Button variant="danger" icon="trash" onclick={() => onremove(t)}>Supprimer</Button>
      {:else}
        <span class="xs faint">Aucune action possible sur un torrent non géré.</span>
      {/if}
    {/snippet}
  </Sheet>
{/if}

<style>
  dl { margin: 0; display: grid; grid-template-columns: 1fr 1fr; gap: 0.9rem 1rem; padding: 1rem 0; border-block: 1px solid var(--border); }
  .wide { grid-column: 1 / -1; }
  dt { font-size: 0.6875rem; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.05em; margin-bottom: 0.2rem; }
  dd { margin: 0; font-size: 0.8125rem; font-weight: 550; word-break: break-word; }
  .d { color: var(--chart-dl); } .u { color: var(--chart-ul); }
  h3 { font-size: 0.8125rem; }
  dt { display: flex; align-items: center; gap: 0.5rem; }
  .edit { appearance: none; border: 0; background: transparent; color: var(--text-2); display: inline-flex; align-items: center; gap: 0.25rem; font: inherit; font-size: 0.6875rem; text-transform: none; letter-spacing: 0; cursor: pointer; padding: 0; text-decoration: underline; text-underline-offset: 2px; }
  .edit:hover { color: var(--text); }
  .tight { gap: 0.15rem; }
  .callout strong { font-weight: 650; }
  .nrow { display: flex; flex-direction: column; gap: 0.45rem; padding: 0.7rem 0.8rem; border: 1px solid var(--border); border-radius: 9px; background: var(--surface-2); }
  .nrow.off { opacity: 0.75; }
  .nm { font-weight: 550; font-size: 0.8125rem; min-width: 0; }
  .pip { width: 0.5rem; height: 0.5rem; border-radius: 50%; background: var(--text-3); flex: none; }
  .pip.on { background: var(--ok); }
  .msg { color: var(--text-2); display: flex; gap: 0.35rem; align-items: flex-start; line-height: 1.4; word-break: break-word; }
  .pw { display: flex; align-items: center; gap: 0.6rem; }
  .pw :global(.track) { flex: 1; }
  .pw span { width: 2.6rem; text-align: right; color: var(--text-2); }
  .tech { font-size: 0.8125rem; color: var(--text-2); }
  .tech summary { cursor: pointer; font-weight: 550; margin-bottom: 0.5rem; }
  .ih { word-break: break-all; font-size: 0.75rem; }
</style>

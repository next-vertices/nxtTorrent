<script>
  import { onDestroy } from 'svelte';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import { live } from './live.svelte.js';
  import { bytes, pct } from './format.js';
  import Dialog from './ui/Dialog.svelte';
  import Button from './ui/Button.svelte';
  import Field from './ui/Field.svelte';
  import Input from './ui/Input.svelte';
  import Select from './ui/Select.svelte';
  import Textarea from './ui/Textarea.svelte';
  import Checkbox from './ui/Checkbox.svelte';
  import Progress from './ui/Progress.svelte';
  import Badge from './ui/Badge.svelte';
  import Icon from './ui/Icon.svelte';

  // Le fichier .torrent est créé SUR le nœud (c'est lui qui a les données) : on choisit un nœud, puis un fichier ou un dossier
  // sous sa racine, puis les options. Les adresses de trackers ne sont ajoutées qu'au dernier moment par le master.
  let { onclose, ondone } = $props();

  let step = $state('source'); // source | options | running | done
  const online = $derived((live.nodes ?? []).filter((n) => n.state === 'Online'));
  let nodeId = $state('');
  $effect(() => {
    if (!nodeId && online.length) nodeId = online[0].id;
  });
  const nodeName = $derived(online.find((n) => n.id === nodeId)?.name ?? '');

  // ----- Explorateur de dossiers du nœud -----
  let cwd = $state('');
  let entries = $state([]);
  let truncated = $state(false);
  let loading = $state(false);
  let browseErr = $state('');
  let chosen = $state(null); // { path, dir, name }

  async function browse(path) {
    loading = true;
    browseErr = '';
    try {
      const r = await api('GET', `/nodes/${nodeId}/browse?path=${encodeURIComponent(path)}`);
      cwd = path;
      entries = r.entries;
      truncated = r.truncated;
    } catch (e) {
      browseErr = e.message;
    } finally {
      loading = false;
    }
  }
  // Nœud choisi ou changé : on repart de sa racine.
  let lastNode = '';
  $effect(() => {
    if (nodeId && nodeId !== lastNode) {
      lastNode = nodeId;
      chosen = null;
      browse('');
    }
  });
  const join = (a, b) => (a ? `${a}/${b}` : b);
  const crumbs = $derived(cwd ? cwd.split('/').map((p, i, a) => ({ name: p, path: a.slice(0, i + 1).join('/') })) : []);
  const up = () => browse(cwd.split('/').slice(0, -1).join('/'));

  // ----- Options -----
  let trackers = $state('');
  let isPrivate = $state(false);
  let source = $state('');
  let piece = $state('0');
  let comment = $state('');
  let addToNode = $state(true);
  let formErr = $state('');
  // Une erreur de saisie disparaît dès qu'on corrige ce qui la causait.
  $effect(() => {
    trackers;
    isPrivate;
    formErr = '';
  });
  const PIECES = [
    { value: '0', label: 'Automatique (recommandé)' },
    ...[16384, 32768, 65536, 131072, 262144, 524288, 1048576, 2097152, 4194304, 8388608, 16777216].map((v) => ({
      value: String(v),
      label: v >= 1048576 ? `${v / 1048576} Mio` : `${v / 1024} Kio`,
    })),
  ];

  /** Une adresse par ligne ; une ligne vide ouvre un nouveau palier (essayés dans l'ordre). */
  function parseTiers(text) {
    const tiers = [[]];
    for (const raw of text.split(/\r?\n/)) {
      const l = raw.trim();
      if (l === '') {
        if (tiers[tiers.length - 1].length) tiers.push([]);
      } else tiers[tiers.length - 1].push(l);
    }
    return tiers.filter((t) => t.length);
  }

  // ----- Création -----
  let job = $state(null);
  let timer = null;
  let starting = $state(false);

  async function start() {
    formErr = '';
    const tiers = parseTiers(trackers);
    const all = tiers.flat();
    if (all.length > 20) return (formErr = '20 adresses de tracker au plus.');
    if (all.some((u) => !/^(https?|udp):\/\/\S{3,}$/.test(u))) return (formErr = 'Chaque adresse de tracker doit commencer par http://, https:// ou udp:// et ne pas contenir d’espace.');
    if (isPrivate && all.length === 0) return (formErr = 'Un torrent privé exige au moins un tracker.');
    starting = true;
    try {
      const r = await api('POST', '/torrent-jobs', {
        node_id: nodeId,
        path: chosen.path,
        trackers: tiers,
        private: isPrivate,
        source: isPrivate ? source.trim() : '',
        piece_length: Number(piece),
        comment: comment.trim(),
        add_to_node: addToNode,
      });
      step = 'running';
      job = { id: r.id, state: 'running', done: 0, total: 0 };
      poll();
    } catch (e) {
      formErr = e.message;
    } finally {
      starting = false;
    }
  }

  function poll() {
    clearTimeout(timer);
    timer = setTimeout(async () => {
      try {
        job = await api('GET', `/torrent-jobs/${job.id}`, undefined, { passive: true });
      } catch (e) {
        job = { ...job, state: 'failed', error: e.message };
      }
      if (job.state === 'running') poll();
      else {
        step = 'done';
        if (job.state === 'done' && job.added) ondone?.();
      }
    }, 1000);
  }

  async function cancel() {
    try {
      await api('DELETE', `/torrent-jobs/${job.id}`);
      toast.success('Création annulée.');
    } catch (e) {
      toast.error(e.message);
    }
    finish();
  }

  /** Ferme l'assistant et libère la tâche côté master (le fichier n'y reste pas en mémoire). */
  async function finish() {
    clearTimeout(timer);
    if (job && job.state !== 'running') {
      try {
        await api('DELETE', `/torrent-jobs/${job.id}`);
      } catch {
        /* déjà oubliée */
      }
    }
    onclose?.();
  }
  onDestroy(() => clearTimeout(timer));

  async function copy(text, what) {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(`${what} copié.`);
    } catch {
      toast.error('Copie impossible dans ce navigateur.');
    }
  }

  const title = $derived({ source: 'Créer un torrent : fichiers', options: 'Créer un torrent : options', running: 'Création en cours…', done: job?.state === 'done' ? 'Torrent créé' : 'Création terminée' }[step]);
</script>

<Dialog {title} size="lg" onclose={finish}>
  {#if step === 'source'}
    <div class="stack-lg">
      <p class="small muted">Le fichier .torrent est calculé <strong>sur le nœud</strong>, qui a les données : choisissez le nœud, puis le fichier ou le dossier à partager.</p>
      {#if online.length === 0}
        <p class="callout warn" role="status"><Icon name="info" size={15} /><span>Aucun nœud en ligne : la création demande un nœud connecté.</span></p>
      {:else}
        <Field label="Nœud">
          <Select bind:value={nodeId} label="Nœud" options={online.map((n) => ({ value: n.id, label: n.name }))} />
        </Field>

        <div class="browser" role="group" aria-label="Fichiers du nœud {nodeName}">
          <div class="crumbs">
            <button type="button" class="crumb" onclick={() => browse('')} aria-label="Racine du nœud"><Icon name="home" size={13} /></button>
            {#each crumbs as c (c.path)}
              <Icon name="chevron-right" size={12} class="sep" />
              <button type="button" class="crumb" onclick={() => browse(c.path)}>{c.name}</button>
            {/each}
          </div>
          {#if browseErr}
            <p class="callout bad" role="alert"><Icon name="alert" size={14} /><span>{browseErr}</span></p>
          {:else if loading}
            <p class="faint small pad">Lecture du dossier…</p>
          {:else}
            <ul class="list">
              {#if cwd}
                <li><button type="button" class="row-btn" onclick={up}><Icon name="chevron-left" size={14} /><span>Dossier parent</span></button></li>
              {/if}
              {#each entries as e (e.name)}
                <li class:sel={chosen?.path === join(cwd, e.name)}>
                  {#if e.link}
                    <span class="row-btn off" title="Lien symbolique : jamais suivi"><Icon name="link" size={14} /><span class="truncate">{e.name}</span><span class="xs faint">lien</span></span>
                  {:else if e.dir}
                    <button type="button" class="row-btn" onclick={() => browse(join(cwd, e.name))} ondblclick={() => (chosen = { path: join(cwd, e.name), dir: true, name: e.name })}>
                      <Icon name="folder" size={14} /><span class="truncate">{e.name}</span>
                    </button>
                    <button type="button" class="pick" onclick={() => (chosen = { path: join(cwd, e.name), dir: true, name: e.name })} aria-label="Choisir le dossier {e.name}">Choisir</button>
                  {:else}
                    <button type="button" class="row-btn" onclick={() => (chosen = { path: join(cwd, e.name), dir: false, name: e.name })} aria-label="Choisir le fichier {e.name}">
                      <Icon name="file" size={14} /><span class="truncate">{e.name}</span><span class="xs faint tnum">{bytes(e.size)}</span>
                    </button>
                  {/if}
                </li>
              {:else}
                <li class="faint small pad">Dossier vide.</li>
              {/each}
              {#if truncated}<li class="faint small pad">Liste tronquée (2 000 entrées affichées).</li>{/if}
            </ul>
          {/if}
        </div>
        <p class="small row chosen">
          {#if chosen}<Icon name={chosen.dir ? 'folder' : 'file'} size={14} /> <span><strong>{chosen.path}</strong> <span class="faint">({chosen.dir ? 'dossier' : 'fichier'})</span></span>{:else}<span class="faint">Rien de choisi : cliquez un fichier, ou « Choisir » à côté d’un dossier.</span>{/if}
        </p>
      {/if}
    </div>
  {:else if step === 'options'}
    <form id="ctform" class="stack-lg" onsubmit={(e) => { e.preventDefault(); start(); }}>
      <p class="small row chosen"><Icon name={chosen.dir ? 'folder' : 'file'} size={14} /> <span><strong>{chosen.path}</strong> sur <strong>{nodeName}</strong></span></p>
      <Field label="Trackers" optional hint="Une adresse par ligne (http, https ou udp). Une ligne vide ouvre un nouveau palier, essayé si le précédent échoue. L'adresse n'est pas enregistrée ni journalisée (elle peut contenir un passkey).">
        <Textarea bind:value={trackers} mono rows={4} placeholder={'https://tracker.exemple.org/announce?passkey=…'} spellcheck="false" autocomplete="off" aria-label="Adresses des trackers" />
      </Field>
      <Checkbox bind:checked={isPrivate}>Torrent privé <span class="faint xs">(pas de DHT ni d'échange de pairs ; exige un tracker)</span></Checkbox>
      {#if isPrivate}
        <Field label="Étiquette « source »" optional hint="Utilisée par certains trackers privés : elle change l'identifiant du torrent d'un tracker à l'autre pour les mêmes fichiers.">
          <Input bind:value={source} maxlength="64" autocomplete="off" aria-label="Étiquette source" />
        </Field>
      {/if}
      <div class="grid2">
        <Field label="Taille des pièces" hint="Automatique convient presque toujours.">
          <Select bind:value={piece} options={PIECES} label="Taille des pièces" />
        </Field>
        <Field label="Commentaire" optional><Input bind:value={comment} maxlength="500" autocomplete="off" aria-label="Commentaire" /></Field>
      </div>
      <Checkbox bind:checked={addToNode}>Ajouter au nœud et partager tout de suite <span class="faint xs">(le client vérifie les fichiers présents puis partage)</span></Checkbox>
      {#if formErr}<p class="callout bad" role="alert"><Icon name="alert" size={14} /><span>{formErr}</span></p>{/if}
    </form>
  {:else if step === 'running'}
    <div class="stack-lg">
      <p class="small muted">Le nœud <strong>{nodeName}</strong> lit et calcule les empreintes de <strong>{chosen.path}</strong>. Cela peut durer longtemps pour de gros contenus.</p>
      <div class="pw"><Progress value={job.total ? job.done / job.total : 0} tone="info" label="Progression du hachage" /><span class="xs tnum">{job.total ? pct(job.done / job.total) : '…'}</span></div>
      {#if job.total}<p class="xs faint tnum">{bytes(job.done)} sur {bytes(job.total)}</p>{/if}
    </div>
  {:else if job?.state === 'done'}
    <div class="stack-lg">
      <p class="callout ok" role="status"><Icon name="check" size={15} /><span>Le fichier .torrent est prêt.</span></p>
      <dl class="meta">
        <div><dt>Nom</dt><dd>{job.name}</dd></div>
        <div><dt>Taille</dt><dd class="tnum">{bytes(job.size)}</dd></div>
        <div><dt>Type</dt><dd>{#if job.private}<Badge tone="warn">privé</Badge>{:else}<Badge>public</Badge>{/if}</dd></div>
        <div class="wide"><dt>Infohash</dt><dd class="row"><code class="mono">{job.infohash}</code><Button variant="ghost" size="icon" icon="copy" label="Copier l'infohash" onclick={() => copy(job.infohash, 'Infohash')} /></dd></div>
      </dl>
      {#if job.added === true}
        <p class="callout ok" role="status"><Icon name="check" size={15} /><span>Ajouté à <strong>{nodeName}</strong> : le client vérifie les fichiers puis partage. Retrouvez-le dans la liste des torrents.</span></p>
      {:else if job.added === false}
        <p class="callout warn" role="status"><Icon name="alert" size={15} /><span>Le torrent est créé mais n'a pas pu être ajouté au nœud : {job.add_error}. Téléchargez-le et ajoutez-le vous-même.</span></p>
      {/if}
      <div class="row wrap">
        <a class="dl" href={`/api/torrent-jobs/${job.id}/file`} download><Icon name="download" size={15} /> Télécharger le .torrent</a>
        <Button variant="outline" icon="copy" onclick={() => copy(job.magnet, 'Lien magnet')}>Copier le lien magnet</Button>
      </div>
      <p class="xs faint">Le fichier n'est conservé que le temps de cette fenêtre : il disparaît du master à sa fermeture.</p>
    </div>
  {:else}
    <div class="stack-lg">
      <p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{job?.state === 'cancelled' ? 'Création annulée.' : `La création a échoué : ${job?.error || 'raison inconnue'}.`}</span></p>
    </div>
  {/if}

  {#snippet footer()}
    {#if step === 'source'}
      <Button variant="outline" onclick={finish}>Annuler</Button>
      <Button variant="primary" disabled={!chosen} onclick={() => (step = 'options')}>Suivant</Button>
    {:else if step === 'options'}
      <Button variant="outline" onclick={() => (step = 'source')}>Retour</Button>
      <Button variant="primary" type="submit" form="ctform" loading={starting}>Créer le .torrent</Button>
    {:else if step === 'running'}
      <Button variant="outline" onclick={cancel}>Annuler la création</Button>
    {:else}
      <Button variant="primary" onclick={finish}>Fermer</Button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .browser { border: 1px solid var(--border); border-radius: var(--radius-sm); overflow: hidden; background: var(--surface); }
  .crumbs { display: flex; align-items: center; gap: 0.15rem; flex-wrap: wrap; padding: 0.45rem 0.6rem; border-bottom: 1px solid var(--border); background: var(--surface-2); }
  .crumbs :global(.sep) { color: var(--text-3); }
  .crumb { appearance: none; border: 0; background: transparent; color: var(--text-2); cursor: pointer; font: inherit; font-size: 0.8125rem; padding: 0.15rem 0.35rem; border-radius: 5px; display: inline-flex; align-items: center; }
  .crumb:hover { background: var(--surface-3); color: var(--text); }
  .list { list-style: none; margin: 0; padding: 0; max-height: 17rem; overflow: auto; }
  .list li { display: flex; align-items: center; border-top: 1px solid var(--border); }
  .list li:first-child { border-top: 0; }
  .list li.sel { background: color-mix(in srgb, var(--info) 12%, transparent); }
  .row-btn { appearance: none; border: 0; background: transparent; color: var(--text); font: inherit; font-size: 0.8125rem; cursor: pointer; display: flex; align-items: center; gap: 0.5rem; flex: 1; min-width: 0; padding: 0.5rem 0.7rem; text-align: left; }
  button.row-btn:hover { background: var(--surface-2); }
  .row-btn.off { color: var(--text-3); cursor: not-allowed; }
  .pick { appearance: none; border: 1px solid var(--border-strong); background: var(--surface); color: var(--text); border-radius: 6px; font: inherit; font-size: 0.75rem; padding: 0.15rem 0.55rem; margin-right: 0.5rem; cursor: pointer; }
  .pick:hover { border-color: var(--ring); }
  .pad { padding: 0.8rem 1rem; }
  .chosen { gap: 0.4rem; align-items: center; }
  .grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 0.8rem; }
  @media (max-width: 560px) { .grid2 { grid-template-columns: 1fr; } }
  .pw { display: flex; align-items: center; gap: 0.7rem; }
  .meta { display: grid; grid-template-columns: repeat(3, 1fr); gap: 0.8rem 1.2rem; margin: 0; }
  .meta .wide { grid-column: 1 / -1; }
  .meta dt { font-size: 0.7rem; color: var(--text-3); text-transform: uppercase; letter-spacing: 0.04em; }
  .meta dd { margin: 0.15rem 0 0; font-size: 0.8125rem; word-break: break-all; }
  .dl { display: inline-flex; align-items: center; gap: 0.4rem; height: 2.125rem; padding: 0 0.9rem; background: var(--text); color: var(--bg); border-radius: var(--radius-sm); font-size: 0.8125rem; font-weight: 600; text-decoration: none; }
  .dl:hover { opacity: 0.9; }
</style>

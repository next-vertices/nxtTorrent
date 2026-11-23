<script>
  import { tr } from './i18n.js';
  import { api, upload, qs, ApiError } from './api.js';
  import { bytes } from './format.js';
  import Dialog from './ui/Dialog.svelte';
  import Button from './ui/Button.svelte';
  import Segmented from './ui/Segmented.svelte';
  import Field from './ui/Field.svelte';
  import Input from './ui/Input.svelte';
  import Textarea from './ui/Textarea.svelte';
  import Icon from './ui/Icon.svelte';

  let { onclose, ondone } = $props();

  const MAX_FILE = 4 * 1024 * 1024; // identique à la limite du serveur
  let mode = $state('file');
  let links = $state('');
  let subdir = $state('');
  let requires = $state('');
  let group = $state('');
  let advanced = $state(false);
  let items = $state([]); // [{ id, label, status, message }]
  let files = $state([]); // [{ id, file }]
  let busy = $state(false);
  let drag = $state(false);
  let seq = 0;

  function addFiles(list) {
    for (const f of list) {
      const id = ++seq;
      if (!/\.torrent$/i.test(f.name)) {
        items.push({ id, kind: 'file', label: f.name, status: 'error', message: tr('Ce n\'est pas un fichier .torrent') });
      } else if (f.size > MAX_FILE) {
        items.push({ id, kind: 'file', label: f.name, status: 'error', message: tr('Trop volumineux ({size}, maximum 4 Mo)', { size: bytes(f.size) }) });
      } else {
        items.push({ id, kind: 'file', label: f.name, status: 'queued', message: bytes(f.size) });
        files.push({ id, file: f });
      }
    }
  }

  function onDrop(e) {
    e.preventDefault();
    drag = false;
    addFiles(e.dataTransfer?.files ?? []);
  }

  function setItem(id, patch) {
    const it = items.find((i) => i.id === id);
    if (it) Object.assign(it, patch);
  }

  const options = () => ({
    save_subdir: subdir.trim(),
    requires: requires.trim().split(/[\s,]+/).filter(Boolean).join(','),
    replicate_group: group.trim(),
  });

  async function sendOne(job) {
    setItem(job.id, { status: 'uploading', message: tr('Envoi…') });
    try {
      await job.run();
      setItem(job.id, { status: 'ok', message: tr('Ajouté') });
    } catch (e) {
      setItem(job.id, { status: 'error', message: e instanceof ApiError ? e.message : tr('Échec') });
    }
  }

  async function submit(e) {
    e.preventDefault();
    const o = options();
    const jobs = [];
    if (mode === 'file') {
      for (const { id, file } of files) {
        jobs.push({ id, run: () => upload(`/torrents/file${qs(o)}`, file) });
      }
    } else {
      // Les lignes en échec d'un essai précédent sont rejouées : on repart d'une liste propre.
      items = items.filter((i) => i.kind !== 'link' || i.status === 'ok');
      for (const line of links.split('\n').map((l) => l.trim()).filter(Boolean)) {
        const id = ++seq;
        const isMagnet = /^magnet:/i.test(line);
        items.push({ id, kind: 'link', line, label: isMagnet ? line.slice(0, 64) + (line.length > 64 ? '…' : '') : line, status: 'queued', message: isMagnet ? tr('Lien magnet') : 'Infohash' });
        const body = {
          [isMagnet ? 'magnet' : 'infohash']: line,
          save_subdir: o.save_subdir,
          requires: o.requires ? o.requires.split(',') : [],
          replicate_group: o.replicate_group || null,
        };
        jobs.push({ id, line, run: () => api('POST', '/torrents', body) });
      }
    }
    if (!jobs.length) return;
    busy = true;
    // 3 envois en parallèle : assez rapide pour des dizaines de fichiers sans saturer le master.
    const queue = [...jobs];
    await Promise.all(Array.from({ length: Math.min(3, queue.length) }, async () => {
      for (let j = queue.shift(); j; j = queue.shift()) await sendOne(j);
    }));
    busy = false;
    files = files.filter((f) => items.find((i) => i.id === f.id)?.status !== 'ok');
    if (mode === 'link') {
      links = jobs.filter((j) => items.find((i) => i.id === j.id)?.status === 'error').map((j) => j.line).join('\n');
    }
    const ok = items.filter((i) => i.status === 'ok').length;
    if (ok) ondone(ok);
    if (!items.some((i) => i.status === 'error')) onclose();
  }

  const pending = $derived(mode === 'file' ? files.length : links.split('\n').filter((l) => l.trim()).length);
</script>

<Dialog title={tr('Ajouter des torrents')} description={tr('Un fichier .torrent est indispensable pour les trackers privés ; un lien magnet suffit pour les trackers publics.')} size="lg" {onclose}>
  <form id="add" class="stack-lg" onsubmit={submit}>
    <Segmented bind:value={mode} label={tr("Type d'ajout")} options={[{ value: 'file', label: tr('Fichiers .torrent') }, { value: 'link', label: tr('Liens magnet / infohash') }]} />

    <!-- Les deux volets occupent la MÊME cellule de grille : la hauteur est celle du plus grand, et basculer d'un onglet à
         l'autre ne fait plus sauter la boîte. Le volet inactif est masqué et inerte (ni focus, ni lecteur d'écran). -->
    <div class="panes">
      <div class="pane" class:off={mode !== 'file'} inert={mode !== 'file'}>
        <label class="drop" class:drag ondragover={(e) => { e.preventDefault(); drag = true; }} ondragleave={() => (drag = false)} ondrop={onDrop}>
          <input type="file" accept=".torrent,application/x-bittorrent" multiple onchange={(e) => { addFiles(e.currentTarget.files); e.currentTarget.value = ''; }} />
          <Icon name="upload" size={22} />
          <strong>{tr('Glissez vos fichiers .torrent ici')}</strong>
          <span class="muted small">{tr('ou cliquez pour les choisir — plusieurs à la fois, 4 Mo maximum chacun')}</span>
        </label>
      </div>
      <div class="pane" class:off={mode !== 'link'} inert={mode !== 'link'}>
        <Field label={tr('Liens magnet ou infohash')} hint={tr('Un par ligne. Les infohash (40 caractères hexadécimaux) sont acceptés.')}>
          <Textarea bind:value={links} mono rows={5} placeholder={'magnet:?xt=urn:btih:…\n1111111111111111111111111111111111111111'} spellcheck="false" aria-label={tr('Liens magnet')} />
        </Field>
      </div>
    </div>

    {#if items.length}
      <ul class="items" aria-label={tr('Fichiers à envoyer')}>
        {#each items as it (it.id)}
          <li class={it.status}>
            <Icon name={it.status === 'ok' ? 'check' : it.status === 'error' ? 'alert' : it.kind === 'link' ? 'link' : 'file'} size={15} />
            <span class="grow truncate">{it.label}</span>
            <span class="xs">{it.message}</span>
          </li>
        {/each}
      </ul>
    {/if}

    <div class="adv">
      <button type="button" class="toggle" aria-expanded={advanced} onclick={() => (advanced = !advanced)}>
        <Icon name={advanced ? 'chevron-down' : 'chevron-right'} size={14} /> {tr('Options de placement')}
      </button>
      {#if advanced}
        <div class="stack-lg opts">
          <Field label={tr('Dossier de destination')} optional hint={tr('Relatif au dossier racine de chaque nœud (jamais de chemin absolu ni de « .. »).')}>
            <Input bind:value={subdir} placeholder={tr('films/2025')} autocomplete="off" />
          </Field>
          <Field label={tr('Labels requis')} optional hint={tr("Le torrent n'ira que sur un nœud portant tous ces labels. Laissez vide pour n'importe quel nœud.")}>
            <Input bind:value={requires} placeholder={tr('vpn-ch ssd')} autocomplete="off" />
          </Field>
          <Field label={tr('Groupe de réplication')} optional hint={tr('À la fin du téléchargement, une copie est placée sur les nœuds portant ce label.')}>
            <Input bind:value={group} placeholder={tr('miroir')} autocomplete="off" />
          </Field>
        </div>
      {/if}
    </div>
  </form>
  {#snippet footer()}
    <Button variant="outline" onclick={onclose}>{items.some((i) => i.status === 'ok') ? tr('Fermer') : tr('Annuler')}</Button>
    <Button variant="primary" type="submit" form="add" loading={busy} disabled={!pending}>
      {tr('Ajouter')}{pending > 1 ? ` (${pending})` : ''}
    </Button>
  {/snippet}
</Dialog>

<style>
  .panes { display: grid; }
  .pane { grid-area: 1 / 1; min-width: 0; display: flex; flex-direction: column; }
  .pane.off { visibility: hidden; }
  /* La zone de dépôt remplit la hauteur du volet (celle du champ des liens). */
  .pane > .drop { flex: 1; justify-content: center; }
  .drop {
    position: relative; display: flex; flex-direction: column; align-items: center; gap: 0.3rem; text-align: center;
    padding: 1.8rem 1rem; border: 1.5px dashed var(--border-strong); border-radius: 12px; cursor: pointer; color: var(--text-2);
    background: var(--surface-2); transition: border-color 0.12s, background 0.12s;
  }
  .drop:hover, .drop.drag { border-color: var(--ring); background: var(--surface-3); }
  .drop strong { color: var(--text); font-size: 0.875rem; margin-top: 0.3rem; }
  .drop input { position: absolute; inset: 0; opacity: 0; cursor: pointer; width: 100%; height: 100%; }
  .items { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; max-height: 12rem; overflow: auto; }
  .items li { display: flex; align-items: center; gap: 0.6rem; padding: 0.4rem 0.6rem; border-radius: 7px; background: var(--surface-2); font-size: 0.8125rem; }
  .items li.ok { color: var(--ok); background: var(--ok-bg); }
  .items li.error { color: var(--bad); background: var(--bad-bg); }
  .items li.uploading { color: var(--info); }
  .toggle { appearance: none; border: 0; background: transparent; color: var(--text-2); display: inline-flex; align-items: center; gap: 0.3rem; font-size: 0.8125rem; font-weight: 550; cursor: pointer; padding: 0; }
  .toggle:hover { color: var(--text); }
  .opts { margin-top: 0.85rem; }
</style>

<script>
  import { onDestroy, onMount } from 'svelte';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Select from './ui/Select.svelte';
  import Segmented from './ui/Segmented.svelte';
  import Icon from './ui/Icon.svelte';

  let mode = $state('script'); // script | code
  let name = $state('');
  let labels = $state('');
  let error = $state('');
  let busy = $state(false);
  let result = $state(null); // { code?, script?, command?, expires_at, labels, name, binary_configured }
  let now = $state(Math.floor(Date.now() / 1000));
  let host = $state(`${location.hostname}:7443`);

  // Script d'installation
  let info = $state(null);
  let masterAddr = $state(`${location.hostname}:7443`);
  let client = $state('transmission');
  let clientUrl = $state('http://127.0.0.1:9091');
  let clientUser = $state('admin');
  let root = $state('/srv/downloads');
  let urlTouched = false;
  const DEFAULT_URL = { transmission: 'http://127.0.0.1:9091', qbittorrent: 'http://127.0.0.1:8080', deluge: 'http://127.0.0.1:8112', rutorrent: 'http://127.0.0.1:8080' };
  const CLIENTS = [
    { value: 'transmission', label: 'Transmission' },
    { value: 'qbittorrent', label: 'qBittorrent' },
    { value: 'deluge', label: 'Deluge' },
    { value: 'rutorrent', label: 'ruTorrent / rTorrent' },
  ];
  $effect(() => {
    // Le port par défaut suit le client choisi, tant que l'utilisateur n'a pas modifié l'adresse.
    if (!urlTouched) clientUrl = DEFAULT_URL[client];
  });

  onMount(async () => {
    try {
      info = await api('GET', '/info');
      masterAddr = `${info.agent_hosts?.[0] ?? location.hostname}:${info.agent_port}`;
      host = masterAddr;
    } catch {
      /* les valeurs par défaut restent utilisables */
    }
  });

  const timer = setInterval(() => (now = Math.floor(Date.now() / 1000)), 1000);
  onDestroy(() => clearInterval(timer));

  const left = $derived(result ? Math.max(0, result.expires_at - now) : 0);
  const mmss = $derived(`${Math.floor(left / 60)}:${String(left % 60).padStart(2, '0')}`);
  const command = $derived(result?.code ? `agent pair ${host.trim() || '<adresse-du-master>:7443'} ${result.code}` : '');
  const labelList = () => labels.split(/[\s,]+/).filter(Boolean);

  async function generate(e) {
    e.preventDefault();
    error = '';
    busy = true;
    try {
      const common = { name: name.trim() || null, labels: labelList() };
      result =
        mode === 'code'
          ? await api('POST', '/pairing-codes', common)
          : { ...(await api('POST', '/agent-install', { ...common, master_addr: masterAddr.trim(), client, client_url: clientUrl.trim(), client_user: clientUser.trim(), root: root.trim() })), ...common };
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  async function copy(text, what) {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(`${what} copié.`);
    } catch {
      toast.error('Copie impossible dans ce navigateur.');
    }
  }
  function download() {
    const blob = new Blob([result.script], { type: 'text/x-shellscript' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'nxt-install-agent.sh';
    a.click();
    URL.revokeObjectURL(a.href);
  }

  // Le code (ou le script qui le contient) disparaît de l'écran dès son expiration : il n'est plus utilisable.
  $effect(() => {
    if (result && left === 0) result = null;
  });
  $effect(() => {
    mode; // changer de mode efface un résultat périmé
    result = null;
    error = '';
  });
</script>

<div class="stack-lg narrow">
  <div>
    <h1>Ajouter un nœud</h1>
    <p class="muted">Un code est valable 15 minutes et ne sert qu'une fois. Il contient l'empreinte de la CA du master : l'agent s'en sert pour vérifier qu'il parle bien à votre master.</p>
  </div>

  <Segmented bind:value={mode} label="Méthode" options={[{ value: 'script', label: 'Installer sur un serveur Linux' }, { value: 'code', label: 'Code seul (installation manuelle)' }]} />

  <form class="card card-pad stack-lg" onsubmit={generate}>
    <Field label="Nom du nœud" optional hint="Ce que vous verrez dans l'interface (ex. « NAS du salon », « Seedbox Hetzner »). Modifiable ensuite.">
      <Input bind:value={name} placeholder="Seedbox Hetzner" maxlength="64" autocomplete="off" />
    </Field>
    <Field label="Labels" optional hint="Ils servent à réserver certains torrents à certains nœuds (ex. vpn-ch, ssd). Sans label, le nœud reçoit les torrents qui n'exigent rien. Ils sont fixés ici par vous, jamais par le nœud lui-même.">
      <Input bind:value={labels} placeholder="ssd vpn-ch" autocomplete="off" />
    </Field>

    {#if mode === 'script'}
      <div class="sep"></div>
      <Field label="Adresse du master, vue depuis le serveur" hint="Nom ou IP + port des agents. Doit être joignable depuis la machine à installer (pare-feu ouvert sur ce port).">
        <Input bind:value={masterAddr} mono placeholder="master.exemple.org:7443" autocomplete="off" required />
      </Field>
      <div class="two">
        <Field label="Client torrent installé sur ce serveur"><Select bind:value={client} options={CLIENTS} label="Client torrent" /></Field>
        <Field label="Adresse de son interface" hint="Toujours sur la machine elle-même (127.0.0.1)."><Input bind:value={clientUrl} mono oninput={() => (urlTouched = true)} autocomplete="off" required /></Field>
      </div>
      <div class="two">
        <Field label="Utilisateur de son interface" optional><Input bind:value={clientUser} autocomplete="off" /></Field>
        <Field label="Dossier des téléchargements" hint="Chemin absolu : tous les chemins du master y sont relatifs."><Input bind:value={root} mono autocomplete="off" required /></Field>
      </div>
      <p class="callout small"><Icon name="info" size={15} /><span>Le mot de passe de l'interface du client ne passe jamais par cette page : le script le demandera sur le serveur, sans l'afficher.</span></p>
    {/if}

    {#if error}<p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>{error}</span></p>{/if}
    <div><Button type="submit" variant="primary" icon={mode === 'script' ? 'download' : 'key'} loading={busy}>{mode === 'script' ? "Générer la commande d'installation" : 'Générer un code'}</Button></div>
  </form>

  {#if result?.command}
    <section class="card result stack-lg" aria-live="polite">
      <header class="row between">
        <h2>Commande d'installation</h2>
        <Badge tone="warn" dot>expire dans {mmss}</Badge>
      </header>
      <ol class="steps">
        <li>
          Connectez-vous en SSH au serveur, puis collez cette commande (elle contient le script complet et le code d'appairage) :
          <div class="codebox">
            <code class="code mono small-code">{result.command.slice(0, 96)}…</code>
            <Button variant="outline" size="sm" icon="copy" onclick={() => copy(result.command, 'Commande')}>Copier</Button>
          </div>
          <div class="row wrap">
            <Button variant="ghost" size="sm" icon="download" onclick={download}>Télécharger le script</Button>
          </div>
        </li>
        <li>
          {#if result.binary_configured}
            Le script télécharge l'agent et <strong>vérifie son empreinte SHA-256</strong> avant de l'installer.
          {:else}
            <strong>Le binaire de l'agent n'est pas téléchargeable depuis ce master.</strong> Copiez-le d'abord sur le serveur (<code>scp nxt-agent serveur:~/</code>), puis lancez la commande avec <code>sudo env NXT_AGENT_BIN=$HOME/nxt-agent sh -c "…"</code>. (L'administrateur peut configurer <code>MASTER_AGENT_BINARY_URL</code> et <code>MASTER_AGENT_BINARY_SHA256</code> pour automatiser cette étape.)
          {/if}
        </li>
        <li>Le script crée un compte système sans privilège, écrit la configuration en 0600, installe un service systemd durci, appaire l'agent, puis le démarre. Le nœud apparaît dans <strong>Nœuds</strong> en quelques secondes.</li>
      </ol>
      <details class="view"><summary>Lire le script avant de l'exécuter</summary><pre class="mono">{result.script}</pre></details>
      <p class="callout"><Icon name="info" size={16} /><span>La commande contient un code à usage unique : ne la partagez pas. Elle n'est affichée qu'ici et le journal n'en garde aucune trace.</span></p>
    </section>
  {:else if result?.code}
    <section class="card result stack-lg" aria-live="polite">
      <header class="row between">
        <h2>Code d'appairage</h2>
        <Badge tone="warn" dot>expire dans {mmss}</Badge>
      </header>

      <div class="codebox">
        <code class="code mono">{result.code}</code>
        <Button variant="outline" size="sm" icon="copy" onclick={() => copy(result.code, 'Code')}>Copier</Button>
      </div>

      {#if result.name || result.labels.length}
        <div class="row wrap">
          {#if result.name}<Badge><Icon name="server" size={11} /> {result.name}</Badge>{/if}
          {#each result.labels as l (l)}<Badge mono>{l}</Badge>{/each}
        </div>
      {/if}

      <ol class="steps">
        <li>
          <strong>Sur la machine à ajouter</strong>, lancez l'agent avec cette commande
          <Field label="Adresse du master (port des agents)">
            <Input bind:value={host} mono placeholder="master.exemple.org:7443" autocomplete="off" />
          </Field>
          <div class="codebox">
            <code class="code mono">{command}</code>
            <Button variant="outline" size="sm" icon="copy" onclick={() => copy(command, 'Commande')}>Copier</Button>
          </div>
        </li>
        <li>Le nœud apparaît dans <strong>Nœuds</strong> dès que l'agent est démarré (<code>agent run</code>).</li>
      </ol>

      <p class="callout"><Icon name="info" size={16} /><span>Ce code n'est affiché qu'une seule fois et n'est conservé nulle part en clair. Il disparaît de cet écran à son expiration.</span></p>
    </section>
  {/if}
</div>

<style>
  .narrow { max-width: 820px; }
  .sep { border-top: 1px solid var(--border); }
  .two { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 1rem; }
  @media (max-width: 640px) { .two { grid-template-columns: minmax(0, 1fr); } }
  .result { padding: 1.1rem 1.25rem; border-color: var(--border-strong); }
  .codebox { display: flex; align-items: center; gap: 0.75rem; background: var(--surface-2); border: 1px dashed var(--border-strong); border-radius: 10px; padding: 0.7rem 0.8rem 0.7rem 1rem; }
  .code { flex: 1; word-break: break-all; line-height: 1.6; user-select: all; font-size: 0.8125rem; }
  .small-code { font-size: 0.75rem; color: var(--text-2); }
  .steps { margin: 0; padding-left: 1.25rem; display: flex; flex-direction: column; gap: 1rem; font-size: 0.875rem; line-height: 1.55; }
  .steps li { display: flex; flex-direction: column; gap: 0.6rem; }
  .steps li::marker { color: var(--text-3); font-weight: 600; }
  .view summary { cursor: pointer; font-size: 0.8125rem; font-weight: 550; color: var(--text-2); }
  .view pre { margin: 0.6rem 0 0; padding: 0.8rem 1rem; background: var(--surface-2); border: 1px solid var(--border); border-radius: 10px; overflow: auto; max-height: 22rem; font-size: 0.72rem; line-height: 1.5; white-space: pre; }
</style>

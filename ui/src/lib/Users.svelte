<script>
  import Rich from './ui/Rich.svelte';
  import { tr } from './i18n.js';
  import { onMount } from 'svelte';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import { ago, dateOnly } from './format.js';
  import { copyText } from './clipboard.js';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Dialog from './ui/Dialog.svelte';
  import ConfirmDialog from './ui/ConfirmDialog.svelte';
  import Menu from './ui/Menu.svelte';
  import Icon from './ui/Icon.svelte';

  let users = $state(null);
  let creating = $state(false);
  let resetting = $state(null); // compte dont on réinitialise le mot de passe
  let removing = $state(null); // compte à supprimer
  let noTotp = $state(null); // compte dont on désactive la double authentification

  async function load() {
    try {
      users = await api('GET', '/users', undefined, { passive: true });
    } catch (e) {
      toast.error(e.message);
    }
  }
  onMount(() => {
    load();
    const t = setInterval(() => !document.hidden && load(), 30000);
    return () => clearInterval(t);
  });

  // Mot de passe provisoire proposé par la page : tiré du générateur sécurisé du navigateur, jamais de Math.random.
  const ALPHABET = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789';
  function generate() {
    const buf = new Uint32Array(20);
    crypto.getRandomValues(buf);
    // Rejet des valeurs qui biaiseraient le tirage (modulo) : chaque caractère est équiprobable.
    const limit = 2 ** 32 - (2 ** 32 % ALPHABET.length);
    let out = '';
    for (const v of buf) if (v < limit) out += ALPHABET[v % ALPHABET.length];
    return out.length >= 16 ? out.slice(0, 16) : generate();
  }

  // ----- Création -----
  let name = $state('');
  let pw = $state('');
  let err = $state('');
  let busy = $state(false);
  let shown = $state(false);
  function openCreate() {
    name = '';
    pw = generate();
    err = '';
    shown = true;
    creating = true;
  }
  async function create(e) {
    e.preventDefault();
    err = '';
    busy = true;
    try {
      await api('POST', '/users', { username: name.trim(), password: pw });
      toast.success(tr('Compte « {name} » créé. Transmettez-lui son mot de passe provisoire.', { name: name.trim() }));
      creating = false;
      await load();
    } catch (e2) {
      err = e2.message;
    } finally {
      busy = false;
    }
  }

  // ----- Réinitialisation -----
  async function reset(e) {
    e.preventDefault();
    err = '';
    busy = true;
    try {
      await api('POST', `/users/${encodeURIComponent(resetting.username)}/password`, { password: pw });
      toast.success(tr('Mot de passe de « {username} » réinitialisé. Ses sessions sont fermées.', { username: resetting.username }));
      resetting = null;
      await load();
    } catch (e2) {
      err = e2.message;
    } finally {
      busy = false;
    }
  }
  function openReset(u) {
    pw = generate();
    err = '';
    shown = true;
    resetting = u;
  }

  async function copy(text) {
    if (await copyText(text)) toast.success(tr('Copié.'));
    else toast.error(tr('Copie impossible : sélectionnez le mot de passe et copiez-le à la main.'));
  }

  // ----- Suppression et double authentification -----
  async function remove() {
    try {
      await api('DELETE', `/users/${encodeURIComponent(removing.username)}`);
      toast.success(tr('Compte « {username} » supprimé.', { username: removing.username }));
    } catch (e) {
      toast.error(e.message);
    }
    removing = null;
    await load();
  }
  async function resetTotp() {
    try {
      await api('DELETE', `/users/${encodeURIComponent(noTotp.username)}/totp`);
      toast.success(tr('Double authentification de « {username} » désactivée. Ses sessions sont fermées.', { username: noTotp.username }));
    } catch (e) {
      toast.error(e.message);
    }
    noTotp = null;
    await load();
  }

  const menu = (u) => [
    { label: tr('Réinitialiser le mot de passe'), icon: 'key', onclick: () => openReset(u), disabled: u.is_me },
    ...(u.totp_enabled ? [{ label: tr('Désactiver la double authentification'), icon: 'shield', onclick: () => (noTotp = u), disabled: u.is_me }] : []),
    { separator: true },
    { label: tr('Supprimer le compte'), icon: 'trash', danger: true, onclick: () => (removing = u), disabled: u.is_me },
  ];
</script>

<div class="stack-lg narrow">
  <div class="head">
    <div>
      <h1>{tr('Utilisateurs')}</h1>
      <p class="muted">{tr('Les personnes qui peuvent administrer ce master. Tous les comptes ont les mêmes droits ; chaque action est attribuée à son auteur dans le journal.')}</p>
    </div>
    <Button variant="primary" icon="plus" onclick={openCreate}>{tr('Créer un compte')}</Button>
  </div>

  <section class="card">
    {#if users === null}
      <p class="faint pad">{tr('Chargement…')}</p>
    {:else}
      <ul class="rows">
        {#each users as u (u.username)}
          <li>
            <span class="ico"><Icon name="user" size={16} /></span>
            <div class="grow">
              <div class="row wrap">
                <strong>{u.username}</strong>
                {#if u.is_me}<Badge tone="info">{tr('vous')}</Badge>{/if}
                {#if u.totp_enabled}<Badge tone="ok" dot>{tr('2FA')}</Badge>{:else}<Badge>{tr('sans 2FA')}</Badge>{/if}
                {#if u.must_change}<Badge tone="warn" dot>{tr('mot de passe provisoire')}</Badge>{/if}
              </div>
              <p class="xs faint">
                {tr('Créé le {date} ·', { date: dateOnly(u.created_at) })}
                {#if u.last_login_at}{tr('dernière connexion {when}', { when: ago(u.last_login_at) })}{:else}{tr('jamais connecté')}{/if}
              </p>
            </div>
            <Menu label={tr('Actions sur le compte {username}', { username: u.username })} items={menu(u)} />
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <p class="callout small"><Icon name="info" size={15} /><span><Rich text={tr('À la création, vous choisissez un mot de passe **provisoire** et le transmettez à la personne. Elle devra le remplacer à sa première connexion, et ne peut rien faire d\'autre d\'ici là. Pour activer sa double authentification, elle va dans **Mon compte**.')} /></span></p>
</div>

{#if creating}
  <Dialog title={tr('Créer un compte')} size="md" onclose={() => (creating = false)}>
    <form id="newuser" class="stack-lg" onsubmit={create}>
      <Field label={tr("Nom d'utilisateur")} hint={tr('3 à 32 caractères : lettres, chiffres, _ . -')}>
        <Input bind:value={name} autocomplete="off" required minlength="3" maxlength="32" />
      </Field>
      <Field label={tr('Mot de passe provisoire')} hint={tr("À transmettre à la personne par un canal sûr. Il n'est affiché qu'ici et ne s'écrit nulle part.")}>
        <div class="pwrow">
          <Input bind:value={pw} mono type={shown ? 'text' : 'password'} autocomplete="off" required aria-label={tr('Mot de passe provisoire')} />
          <Button variant="outline" size="sm" type="button" icon="eye" onclick={() => (shown = !shown)}>{shown ? 'Masquer' : 'Afficher'}</Button>
          <Button variant="outline" size="sm" type="button" icon="refresh" onclick={() => (pw = generate())}>{tr('Générer')}</Button>
          <Button variant="outline" size="sm" type="button" icon="copy" onclick={() => copy(pw)}>{tr('Copier')}</Button>
        </div>
      </Field>
      {#if err}<p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>{err}</span></p>{/if}
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (creating = false)}>{tr('Annuler')}</Button>
      <Button variant="primary" type="submit" form="newuser" loading={busy}>{tr('Créer')}</Button>
    {/snippet}
  </Dialog>
{/if}

{#if resetting}
  <Dialog title={tr('Réinitialiser le mot de passe de {username}', { username: resetting.username })} size="md" onclose={() => (resetting = null)}>
    <form id="resetuser" class="stack-lg" onsubmit={reset}>
      <p class="small muted">{tr('Le compte sera déconnecté partout et devra remplacer ce mot de passe provisoire à sa prochaine connexion.')}</p>
      <Field label={tr('Nouveau mot de passe provisoire')}>
        <div class="pwrow">
          <Input bind:value={pw} mono type={shown ? 'text' : 'password'} autocomplete="off" required aria-label={tr('Nouveau mot de passe provisoire')} />
          <Button variant="outline" size="sm" type="button" icon="eye" onclick={() => (shown = !shown)}>{shown ? 'Masquer' : 'Afficher'}</Button>
          <Button variant="outline" size="sm" type="button" icon="refresh" onclick={() => (pw = generate())}>{tr('Générer')}</Button>
          <Button variant="outline" size="sm" type="button" icon="copy" onclick={() => copy(pw)}>{tr('Copier')}</Button>
        </div>
      </Field>
      {#if err}<p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>{err}</span></p>{/if}
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (resetting = null)}>{tr('Annuler')}</Button>
      <Button variant="primary" type="submit" form="resetuser" loading={busy}>{tr('Réinitialiser')}</Button>
    {/snippet}
  </Dialog>
{/if}

{#if removing}
  <ConfirmDialog title={tr('Supprimer le compte « {username} » ?', { username: removing.username })} confirmLabel={tr('Supprimer')} danger onconfirm={remove} oncancel={() => (removing = null)}>
    <p>{tr('Le compte ne pourra plus se connecter et ses sessions en cours sont fermées. Son nom reste dans le journal des actions passées. Cette action ne peut pas être annulée.')}</p>
  </ConfirmDialog>
{/if}

{#if noTotp}
  <ConfirmDialog title={tr('Désactiver la double authentification de « {username} » ?', { username: noTotp.username })} confirmLabel={tr('Désactiver')} danger onconfirm={resetTotp} oncancel={() => (noTotp = null)}>
    <p><Rich text={tr('À utiliser si la personne a perdu son téléphone et ses codes de secours. Son compte n\'est plus protégé que par son mot de passe jusqu\'à ce qu\'elle réactive la double authentification dans **Mon compte**. Ses sessions en cours sont fermées.')} /></p>
  </ConfirmDialog>
{/if}

<style>
  .narrow { max-width: 820px; }
  .head { display: flex; justify-content: space-between; align-items: flex-start; gap: 1rem; flex-wrap: wrap; }
  .pad { padding: 1rem 1.1rem; }
  .rows { list-style: none; margin: 0; padding: 0.3rem 0.6rem 0.5rem; display: flex; flex-direction: column; }
  .rows li { display: flex; align-items: center; gap: 0.8rem; padding: 0.7rem 0.5rem; border-top: 1px solid var(--border); }
  .rows li:first-child { border-top: 0; }
  .ico { width: 1.9rem; height: 1.9rem; border-radius: 50%; display: grid; place-items: center; flex: none; background: var(--surface-2); color: var(--text-2); border: 1px solid var(--border); }
  .pwrow { display: flex; gap: 0.4rem; align-items: center; flex-wrap: wrap; }
  .pwrow :global(input) { min-width: 12rem; flex: 1; }
</style>

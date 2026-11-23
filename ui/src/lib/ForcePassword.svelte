<script>
  // Premier écran d'un compte créé (ou réinitialisé) par un administrateur : son mot de passe est provisoire, l'administrateur
  // qui l'a choisi le connaît. Rien d'autre n'est accessible tant qu'il n'est pas remplacé (le serveur le refuse aussi).
  import Logo from './ui/Logo.svelte';
  import { api } from './api.js';
  import Button from './ui/Button.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Icon from './ui/Icon.svelte';
  import { tr } from './i18n.js';

  let { user, ondone, onlogout } = $props();

  let current = $state('');
  let next = $state('');
  let again = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(e) {
    e.preventDefault();
    error = '';
    if (next !== again) {
      error = tr('Les deux nouveaux mots de passe ne correspondent pas.');
      return;
    }
    busy = true;
    try {
      await api('POST', '/password', { current, new: next });
      ondone();
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }
</script>

<main>
  <div class="box">
    <div class="brand">
      <Logo size={44} />
      <h1>{tr('nxtTorrent')}</h1>
      <p class="muted small">{tr('Bienvenue, {user}', { user })}</p>
    </div>
    <div class="card card-pad stack-lg">
      <p class="callout"><Icon name="lock" size={16} /><span>{tr("Votre mot de passe est provisoire : l'administrateur qui a créé votre compte le connaît. Choisissez-en un qui n'appartient qu'à vous pour continuer.")}</span></p>
      <form class="stack-lg" onsubmit={submit}>
        <Field label={tr('Mot de passe provisoire')}><Input type="password" bind:value={current} autocomplete="current-password" required /></Field>
        <Field label={tr('Nouveau mot de passe')} hint={tr("12 caractères minimum, avec au moins trois types de caractères — ou une phrase de passe d'au moins 20 caractères.")}>
          <Input type="password" bind:value={next} autocomplete="new-password" required />
        </Field>
        <Field label={tr('Confirmation')}><Input type="password" bind:value={again} autocomplete="new-password" required /></Field>
        {#if error}<p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>{error}</span></p>{/if}
        <Button type="submit" variant="primary" class="full" loading={busy}>{tr('Changer le mot de passe et continuer')}</Button>
      </form>
      <button type="button" class="linkbtn" onclick={onlogout}>{tr('Se déconnecter')}</button>
    </div>
  </div>
</main>

<style>
  main { min-height: 100vh; display: grid; place-items: center; padding: 1.5rem; }
  .box { width: min(420px, 100%); display: flex; flex-direction: column; gap: 1.5rem; }
  .brand { display: flex; flex-direction: column; align-items: center; gap: 0.35rem; text-align: center; }
  .brand h1 { margin-top: 0.4rem; }
  :global(.full) { width: 100%; height: 2.4rem; }
  .linkbtn { appearance: none; border: 0; background: transparent; color: var(--text-2); font: inherit; font-size: 0.8125rem; cursor: pointer; padding: 0; text-decoration: underline; text-underline-offset: 3px; align-self: center; }
  .linkbtn:hover { color: var(--text); }
</style>

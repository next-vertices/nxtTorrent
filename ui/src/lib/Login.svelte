<script>
  import Logo from './ui/Logo.svelte';
  import { api, ApiError } from './api.js';
  import Button from './ui/Button.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Checkbox from './ui/Checkbox.svelte';
  import Icon from './ui/Icon.svelte';
  import LangPicker from './ui/LangPicker.svelte';
  import { tr } from './i18n.js';

  let { setupRequired = false, onsuccess } = $props();

  // La valeur initiale de la prop suffit : ce composant est recréé quand on passe de la connexion à l'initialisation.
  // svelte-ignore state_referenced_locally
  let username = $state(setupRequired ? 'admin' : '');
  let password = $state('');
  let confirm = $state('');
  let error = $state('');
  let busy = $state(false);
  let remember = $state(false);
  // Étape 2 (double authentification) : le mot de passe est validé, on attend le code.
  let ticket = $state('');
  let code = $state('');
  let useRecovery = $state(false);

  function explain(err) {
    if (err instanceof ApiError && err.status === 429) return tr('Trop de tentatives. Réessayez dans {s} s.', { s: err.retryAfter || 60 });
    if (err instanceof ApiError && err.status === 401 && !ticket) return tr('Identifiants invalides.');
    return err.message;
  }

  async function submit(e) {
    e.preventDefault();
    error = '';
    if (setupRequired && password !== confirm) {
      error = tr('Les deux mots de passe ne correspondent pas.');
      return;
    }
    busy = true;
    try {
      if (setupRequired) await api('POST', '/setup', { username, password });
      const r = await api('POST', '/login', { username, password, remember });
      if (r?.needs_totp) {
        ticket = r.ticket;
        password = '';
        return;
      }
      onsuccess(username);
    } catch (err) {
      error = explain(err);
    } finally {
      busy = false;
    }
  }

  async function submitCode(e) {
    e.preventDefault();
    error = '';
    busy = true;
    try {
      const r = await api('POST', '/login/totp', { ticket, code });
      onsuccess(r.username ?? username);
    } catch (err) {
      error = explain(err);
      code = '';
      // Connexion expirée ou ticket épuisé : on repart du mot de passe.
      if (err instanceof ApiError && err.status === 401 && /expir/.test(err.message) && attemptsLeft() <= 0) ticket = '';
    } finally {
      busy = false;
    }
  }
  let failures = $state(0);
  const attemptsLeft = () => 5 - ++failures;
  function back() {
    ticket = '';
    code = '';
    error = '';
    failures = 0;
    useRecovery = false;
  }
</script>

<main>
  <div class="box">
    <div class="brand">
      <Logo size={44} />
      <h1>{tr('nxtTorrent')}</h1>
      <p class="muted small">{ticket ? tr('Vérification en deux étapes') : setupRequired ? tr('Première utilisation') : tr('Administration')}</p>
    </div>

    <div class="card card-pad stack-lg">
      {#if ticket}
        <p class="callout"><Icon name="lock" size={16} /><span>{useRecovery ? tr('Saisissez un de vos codes de secours (usage unique).') : tr("Saisissez le code à 6 chiffres affiché par votre application d'authentification.")}</span></p>
        <form class="stack-lg" onsubmit={submitCode}>
          <Field label={useRecovery ? tr('Code de secours') : tr('Code à 6 chiffres')}>
            <Input bind:value={code} mono autocomplete="one-time-code" inputmode={useRecovery ? 'text' : 'numeric'} placeholder={useRecovery ? 'XXXXX-XXXXX' : '123 456'} required aria-label={tr('Code de vérification')} />
          </Field>
          {#if error}<p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>{error}</span></p>{/if}
          <Button type="submit" variant="primary" class="full" loading={busy}>{tr('Vérifier')}</Button>
          <div class="row between">
            <button type="button" class="linkbtn" onclick={() => { useRecovery = !useRecovery; code = ''; error = ''; }}>{useRecovery ? tr("Utiliser l'application") : tr('Utiliser un code de secours')}</button>
            <button type="button" class="linkbtn" onclick={back}>{tr('Retour')}</button>
          </div>
        </form>
      {:else}
        {#if setupRequired}
          <p class="callout"><Icon name="info" size={16} /><span>{tr('Créez le compte administrateur. Vous serez ensuite connecté et pourrez appairer vos premiers nœuds.')}</span></p>
        {/if}

        <form class="stack-lg" onsubmit={submit}>
          <Field label={tr("Nom d'utilisateur")}>
            <Input bind:value={username} autocomplete="username" required minlength="3" maxlength="32" />
          </Field>
          <Field label={tr('Mot de passe')}>
            <Input type="password" bind:value={password} autocomplete={setupRequired ? 'new-password' : 'current-password'} required />
          </Field>
          {#if setupRequired}
            <Field
              label={tr('Confirmation')}
              hint={tr("12 caractères minimum, avec majuscules, minuscules, chiffres ou symboles (au moins trois types) — ou une phrase de passe d'au moins 20 caractères.")}
            >
              <Input type="password" bind:value={confirm} autocomplete="new-password" required />
            </Field>
          {/if}
          <Checkbox bind:checked={remember}>{tr('Rester connecté sur cet appareil')}</Checkbox>
          {#if error}<p class="callout bad" role="alert"><Icon name="alert" size={16} /><span>{error}</span></p>{/if}
          <Button type="submit" variant="primary" class="full" loading={busy}>{setupRequired ? tr('Créer le compte') : tr('Se connecter')}</Button>
        </form>
      {/if}
    </div>
    <div class="lang"><LangPicker /></div>
  </div>
</main>

<style>
  main { min-height: 100vh; display: grid; place-items: center; padding: 1.5rem; }
  .box { width: min(400px, 100%); display: flex; flex-direction: column; gap: 1.5rem; }
  .brand { display: flex; flex-direction: column; align-items: center; gap: 0.35rem; text-align: center; }
  .brand h1 { margin-top: 0.4rem; }
  :global(.full) { width: 100%; height: 2.4rem; }
  .linkbtn { appearance: none; border: 0; background: transparent; color: var(--text-2); font: inherit; font-size: 0.8125rem; cursor: pointer; padding: 0; text-decoration: underline; text-underline-offset: 3px; }
  .linkbtn:hover { color: var(--text); }
  .lang { display: flex; justify-content: center; }
</style>

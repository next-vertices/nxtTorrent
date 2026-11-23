<script>
  import Rich from './ui/Rich.svelte';
  import { tr } from './i18n.js';
  import { onMount } from 'svelte';
  import { copyText } from './clipboard.js';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import Button from './ui/Button.svelte';
  import Badge from './ui/Badge.svelte';
  import Input from './ui/Input.svelte';
  import Field from './ui/Field.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Checkbox from './ui/Checkbox.svelte';
  import Icon from './ui/Icon.svelte';

  let acct = $state(null);

  async function load() {
    try {
      acct = await api('GET', '/account');
    } catch (e) {
      toast.error(e.message);
    }
  }
  onMount(load);

  // ----- Mot de passe -----
  let cur = $state('');
  let next = $state('');
  let again = $state('');
  let pwErr = $state('');
  let pwBusy = $state(false);
  async function changePassword(e) {
    e.preventDefault();
    pwErr = '';
    if (next !== again) {
      pwErr = tr('Les deux nouveaux mots de passe ne correspondent pas.');
      return;
    }
    pwBusy = true;
    try {
      await api('POST', '/password', { current: cur, new: next });
      toast.success(tr('Mot de passe modifié. Vos autres sessions ont été fermées.'));
      cur = next = again = '';
    } catch (err) {
      pwErr = err.message;
    } finally {
      pwBusy = false;
    }
  }

  // ----- Double authentification : activation -----
  let setup = $state(null); // { secret, uri, qr }
  let code = $state('');
  let setupErr = $state('');
  let setupBusy = $state(false);
  let recovery = $state(null); // codes de secours, affichés UNE fois
  let saved = $state(false);

  async function startSetup() {
    setupErr = '';
    code = '';
    try {
      setup = await api('POST', '/totp/setup', {});
    } catch (err) {
      toast.error(err.message);
    }
  }
  async function enable(e) {
    e.preventDefault();
    setupErr = '';
    setupBusy = true;
    try {
      const r = await api('POST', '/totp/enable', { code });
      recovery = r.recovery_codes;
      saved = false;
      setup = null;
      await load();
    } catch (err) {
      setupErr = err.message;
    } finally {
      setupBusy = false;
    }
  }
  async function copy(text, what) {
    try {
      if (!(await copyText(text))) throw new Error('copie refusée');
      toast.success(tr('{what} copié.', { what }));
    } catch {
      toast.error(tr('Copie impossible dans ce navigateur.'));
    }
  }
  function download() {
    const blob = new Blob([tr('Codes de secours nxtTorrent (usage unique)') + '\n\n' + recovery.join('\n') + '\n'], { type: 'text/plain' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'nxttorrent-codes-de-secours.txt';
    a.click();
    URL.revokeObjectURL(a.href);
  }

  // ----- Sauvegarde chiffrée -----
  let bkPw = $state('');
  let bkCode = $state('');
  let bkPass = $state('');
  let bkPass2 = $state('');
  let bkErr = $state('');
  let bkBusy = $state(false);
  async function backup(e) {
    e.preventDefault();
    bkErr = '';
    if (bkPass !== bkPass2) {
      bkErr = tr('Les deux phrases de passe ne correspondent pas.');
      return;
    }
    bkBusy = true;
    try {
      // Réponse binaire : le client d'API (JSON) ne convient pas ici.
      const res = await fetch('/api/backup', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json', 'X-Requested-With': 'nxt' },
        body: JSON.stringify({ password: bkPw, code: bkCode, passphrase: bkPass }),
      });
      if (!res.ok) {
        const j = await res.json().catch(() => ({}));
        throw new Error(j.error ?? tr('Erreur {status}', { status: res.status }));
      }
      const name = /filename="([^"]+)"/.exec(res.headers.get('content-disposition') ?? '')?.[1] ?? 'nxttorrent.nxtbak';
      const a = document.createElement('a');
      a.href = URL.createObjectURL(await res.blob());
      a.download = name;
      a.click();
      URL.revokeObjectURL(a.href);
      toast.success(tr('Sauvegarde téléchargée. Conservez-la hors de cette machine, avec sa phrase de passe.'));
      bkPw = bkCode = bkPass = bkPass2 = '';
    } catch (err) {
      bkErr = err.message;
    } finally {
      bkBusy = false;
    }
  }

  // ----- Désactivation -----
  let disabling = $state(false);
  let dPw = $state('');
  let dCode = $state('');
  let dErr = $state('');
  let dBusy = $state(false);
  async function disable(e) {
    e.preventDefault();
    dErr = '';
    dBusy = true;
    try {
      await api('POST', '/totp/disable', { password: dPw, code: dCode });
      toast.success(tr('Double authentification désactivée.'));
      disabling = false;
      dPw = dCode = '';
      await load();
    } catch (err) {
      dErr = err.message;
    } finally {
      dBusy = false;
    }
  }
</script>

<div class="stack-lg narrow">
  <div>
    <h1>{tr('Mon compte')}</h1>
    {#if acct}<p class="muted"><Rich text={tr('Connecté en tant que **{name}**.', { name: acct.username })} /></p>{/if}
  </div>

  <section class="card card-pad stack-lg">
    <div class="row between">
      <div>
        <h2>{tr('Double authentification')}</h2>
        <p class="muted small">{tr("Un code à usage unique, généré par une application sur votre téléphone, s'ajoute au mot de passe : un mot de passe volé ne suffit plus.")}</p>
      </div>
      {#if acct}<Badge tone={acct.totp_enabled ? 'ok' : 'warn'} dot>{acct.totp_enabled ? tr('Activée') : tr('Désactivée')}</Badge>{/if}
    </div>

    {#if recovery}
      <div class="stack" aria-live="polite">
        <p class="callout warn"><Icon name="alert" size={16} /><span><Rich text={tr("**Conservez ces codes de secours maintenant.** Ils ne seront plus jamais affichés. Chacun ne sert qu'une fois, si vous perdez votre téléphone.")} /></span></p>
        <ul class="codes mono">{#each recovery as c (c)}<li>{c}</li>{/each}</ul>
        <div class="row wrap">
          <Button variant="outline" size="sm" icon="copy" onclick={() => copy(recovery.join('\n'), 'Codes')}>{tr('Copier')}</Button>
          <Button variant="outline" size="sm" icon="download" onclick={download}>{tr('Télécharger')}</Button>
        </div>
        <Checkbox bind:checked={saved}>{tr("J'ai conservé ces codes en lieu sûr")}</Checkbox>
        <div><Button variant="primary" disabled={!saved} onclick={() => (recovery = null)}>{tr('Terminé')}</Button></div>
      </div>
    {:else if acct && !acct.totp_enabled}
      {#if !setup}
        <div><Button variant="primary" icon="shield" onclick={startSetup}>{tr('Activer la double authentification')}</Button></div>
      {:else}
        <ol class="steps">
          <li>
            {tr('Scannez ce code avec votre application (Aegis, Google Authenticator, 1Password, Bitwarden…).')}
            <div class="qrrow">
              {#if setup.qr}<img class="qr" src={setup.qr} alt={tr('Code QR de configuration')} width="180" height="180" />{/if}
              <div class="stack">
                <span class="xs faint">{tr('Ou saisissez la clé à la main :')}</span>
                <code class="mono secret">{setup.secret}</code>
                <Button variant="ghost" size="sm" icon="copy" onclick={() => copy(setup.secret, tr('Clé'))}>{tr('Copier la clé')}</Button>
              </div>
            </div>
          </li>
          <li>
            {tr('Saisissez le code à 6 chiffres affiché pour confirmer.')}
            <form class="row" onsubmit={enable}>
              <div class="codein"><Input bind:value={code} mono inputmode="numeric" autocomplete="one-time-code" placeholder="123 456" aria-label={tr('Code à 6 chiffres')} required /></div>
              <Button type="submit" variant="primary" loading={setupBusy}>{tr('Activer')}</Button>
              <Button variant="ghost" onclick={() => (setup = null)}>{tr('Annuler')}</Button>
            </form>
            {#if setupErr}<p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{setupErr}</span></p>{/if}
          </li>
        </ol>
      {/if}
    {:else if acct}
      <p class="small">
        {tr('{n} code de secours restant|{n} codes de secours restants', { n: acct.recovery_remaining })}
        {#if acct.recovery_remaining <= 3}<strong class="warnc">{tr('Il en reste peu : désactivez puis réactivez la double authentification pour en générer de nouveaux.')}</strong>{/if}
      </p>
      <div><Button variant="danger" onclick={() => (disabling = true)}>{tr('Désactiver…')}</Button></div>
    {/if}
  </section>

  <section class="card card-pad">
    <form class="stack-lg" onsubmit={backup}>
      <div>
        <h2>{tr('Sauvegarde')}</h2>
        <p class="muted small">{tr("Un fichier chiffré contenant la base (nœuds, torrents, comptes, alertes), l'autorité de certification et les clés. Sans lui, perdre le disque du master vous obligerait à ré-appairer tous les nœuds.")}</p>
      </div>
      <p class="callout warn small"><Icon name="alert" size={15} /><span><Rich text={tr('Il contient la **clé privée de votre CA** : il est chiffré par la phrase de passe ci-dessous, **qui ne peut pas être récupérée**. Conservez les deux, séparément, hors de cette machine.')} /></span></p>
      <Field label={tr('Mot de passe actuel')}><Input type="password" bind:value={bkPw} autocomplete="current-password" required /></Field>
      {#if acct?.totp_enabled}<Field label={tr('Code de double authentification')}><Input bind:value={bkCode} mono autocomplete="one-time-code" required aria-label={tr('Code de double authentification')} /></Field>{/if}
      <div class="two">
        <Field label={tr('Phrase de passe de la sauvegarde')} hint={tr('12 caractères minimum.')}><Input type="password" bind:value={bkPass} autocomplete="new-password" required aria-label={tr('Phrase de passe de la sauvegarde')} /></Field>
        <Field label={tr('Confirmation')}><Input type="password" bind:value={bkPass2} autocomplete="new-password" required aria-label={tr('Confirmation de la phrase de passe')} /></Field>
      </div>
      {#if bkErr}<p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{bkErr}</span></p>{/if}
      <div><Button type="submit" variant="outline" icon="download" loading={bkBusy}>{tr('Télécharger une sauvegarde chiffrée')}</Button></div>
      <p class="xs faint"><Rich text={tr('Restauration (master arrêté) : `master --data-dir … admin restore --from fichier.nxtbak` — la phrase de passe est lue dans `MASTER_BACKUP_PASSPHRASE`. Sauvegarde automatique possible avec `master admin backup --out …` (cron, systemd timer).')} /></p>
    </form>
  </section>

  <section class="card card-pad">
    <form class="stack-lg" onsubmit={changePassword}>
      <div><h2>{tr('Mot de passe')}</h2><p class="muted small">{tr('Changer votre mot de passe ferme vos autres sessions.')}</p></div>
      <Field label={tr('Mot de passe actuel')}><Input type="password" bind:value={cur} autocomplete="current-password" required /></Field>
      <Field label={tr('Nouveau mot de passe')} hint={tr("12 caractères minimum, avec au moins trois types de caractères — ou une phrase de passe d'au moins 20 caractères.")}><Input type="password" bind:value={next} autocomplete="new-password" required /></Field>
      <Field label={tr('Confirmation')}><Input type="password" bind:value={again} autocomplete="new-password" required /></Field>
      {#if pwErr}<p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{pwErr}</span></p>{/if}
      <div><Button type="submit" variant="primary" loading={pwBusy}>{tr('Changer le mot de passe')}</Button></div>
    </form>
  </section>
</div>

{#if disabling}
  <Dialog title={tr('Désactiver la double authentification')} description={tr('Votre mot de passe ET un code (application ou code de secours) sont exigés : une session volée ne suffit pas à retirer la protection.')} size="sm" onclose={() => (disabling = false)}>
    <form id="dis" class="stack-lg" onsubmit={disable}>
      <Field label={tr('Mot de passe')}><Input type="password" bind:value={dPw} autocomplete="current-password" required /></Field>
      <Field label={tr('Code à 6 chiffres ou code de secours')}><Input bind:value={dCode} mono autocomplete="one-time-code" required aria-label={tr('Code de vérification')} /></Field>
      {#if dErr}<p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{dErr}</span></p>{/if}
    </form>
    {#snippet footer()}
      <Button variant="outline" onclick={() => (disabling = false)}>{tr('Annuler')}</Button>
      <Button variant="danger" class="solid" type="submit" form="dis" loading={dBusy}>{tr('Désactiver')}</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .narrow { max-width: 760px; }
  .steps { margin: 0; padding-left: 1.25rem; display: flex; flex-direction: column; gap: 1.25rem; font-size: 0.875rem; line-height: 1.55; }
  .steps li { display: flex; flex-direction: column; gap: 0.7rem; }
  .steps li::marker { color: var(--text-3); font-weight: 600; }
  .qrrow { display: flex; gap: 1.25rem; align-items: center; flex-wrap: wrap; }
  .qr { background: #fff; padding: 6px; border-radius: 10px; border: 1px solid var(--border); }
  .secret { word-break: break-all; background: var(--surface-2); border: 1px dashed var(--border-strong); padding: 0.45rem 0.65rem; border-radius: 8px; user-select: all; }
  .codein { width: 9rem; }
  .codes { list-style: none; margin: 0; padding: 0.9rem 1rem; display: grid; grid-template-columns: repeat(auto-fill, minmax(9rem, 1fr)); gap: 0.4rem 1rem; background: var(--surface-2); border: 1px solid var(--border); border-radius: 10px; user-select: all; }
  .warnc { color: var(--warn); }
  .two { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 1rem; }
  @media (max-width: 640px) { .two { grid-template-columns: minmax(0, 1fr); } }
</style>

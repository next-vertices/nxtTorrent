<script>
  // Mise à jour d'un agent : le mot de passe est redemandé (c'est un programme qu'on remplace à distance).
  import { onMount } from 'svelte';
  import { api } from './api.js';
  import { toast } from './toast.svelte.js';
  import { tr } from './i18n.js';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import Input from './ui/Input.svelte';
  import Icon from './ui/Icon.svelte';

  let { node, onclose, ondone } = $props();

  let password = $state('');
  let code = $state('');
  let error = $state('');
  let busy = $state(false);
  let totp = $state(false);
  onMount(async () => {
    try {
      totp = (await api('GET', '/account')).totp_enabled;
    } catch {
      /* le serveur réclamera le code s'il le faut */
    }
  });

  async function submit(e) {
    e.preventDefault();
    error = '';
    busy = true;
    try {
      await api('POST', `/nodes/${node.id}/update`, { password, code });
      toast.success(tr('Mise à jour de « {name} » demandée.', { name: node.name }));
      ondone();
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title={tr('Mettre à jour « {name} » vers {version}', { name: node.name, version: node.update.available })} description={tr("Le nœud reçoit la release signée depuis le master, la vérifie, puis redémarre son agent. S'il ne se reconnecte pas, l'ancienne version est rétablie. Le client torrent continue de fonctionner.")} size="md" {onclose}>
  <form id="nodeupdate" class="stack-lg" onsubmit={submit}>
    <Field label={tr('Mot de passe actuel')}><Input type="password" bind:value={password} autocomplete="current-password" required /></Field>
    {#if totp}<Field label={tr('Code de double authentification')}><Input bind:value={code} mono autocomplete="one-time-code" required aria-label={tr('Code de double authentification')} /></Field>{/if}
    {#if error}<p class="callout bad" role="alert"><Icon name="alert" size={15} /><span>{error}</span></p>{/if}
  </form>
  {#snippet footer()}
    <Button variant="outline" onclick={onclose}>{tr('Annuler')}</Button>
    <Button variant="primary" type="submit" form="nodeupdate" loading={busy}>{tr('Mettre à jour')}</Button>
  {/snippet}
</Dialog>

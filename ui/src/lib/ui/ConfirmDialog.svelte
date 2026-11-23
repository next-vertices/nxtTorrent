<script>
  import { tr } from '../i18n.js';
  import Dialog from './Dialog.svelte';
  import Button from './Button.svelte';

  let { title, confirmLabel = 'Confirmer', danger = false, busy = false, onconfirm, oncancel, children } = $props();
</script>

<Dialog {title} size="sm" onclose={oncancel}>
  <div class="content">{@render children?.()}</div>
  {#snippet footer()}
    <Button variant="outline" onclick={oncancel}>{tr('Annuler')}</Button>
    <Button variant={danger ? 'danger' : 'primary'} class={danger ? 'solid' : ''} loading={busy} onclick={onconfirm}>{confirmLabel}</Button>
  {/snippet}
</Dialog>

<style>
  .content { color: var(--text-2); font-size: 0.875rem; display: flex; flex-direction: column; gap: 0.6rem; line-height: 1.5; }
</style>

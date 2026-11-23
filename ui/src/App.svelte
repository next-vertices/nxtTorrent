<script>
  import { tr } from './lib/i18n.js';
  import { onMount } from 'svelte';
  import { api, setUnauthorizedHandler } from './lib/api.js';
  import { live, startLive, stopLive } from './lib/live.svelte.js';
  import { theme, setTheme } from './lib/theme.svelte.js';
  import { num } from './lib/format.js';
  import Login from './lib/Login.svelte';
  import Overview from './lib/Overview.svelte';
  import Nodes from './lib/Nodes.svelte';
  import Torrents from './lib/Torrents.svelte';
  import Pairing from './lib/Pairing.svelte';
  import Audit from './lib/Audit.svelte';
  import Account from './lib/Account.svelte';
  import Alerts from './lib/Alerts.svelte';
  import CrossSeed from './lib/CrossSeed.svelte';
  import Stats from './lib/Stats.svelte';
  import Users from './lib/Users.svelte';
  import ForcePassword from './lib/ForcePassword.svelte';
  import Icon from './lib/ui/Icon.svelte';
  import Logo from './lib/ui/Logo.svelte';
  import LangPicker from './lib/ui/LangPicker.svelte';
  import Button from './lib/ui/Button.svelte';
  import Toaster from './lib/ui/Toaster.svelte';

  const NAV = [
    { id: 'overview', label: tr('Vue d\'ensemble'), icon: 'home' },
    { id: 'torrents', label: tr('Torrents'), icon: 'download' },
    { id: 'nodes', label: tr('Nœuds'), icon: 'server' },
    { id: 'stats', label: tr('Statistiques'), icon: 'activity' },
    { id: 'pairing', label: tr('Appairage'), icon: 'key' },
    { id: 'crossseed', label: tr('Multi-trackers'), icon: 'layers' },
    { id: 'alerts', label: tr('Alertes'), icon: 'alert' },
    { id: 'audit', label: tr('Journal'), icon: 'shield' },
    { id: 'users', label: tr('Utilisateurs'), icon: 'users' },
    { id: 'account', label: tr('Mon compte'), icon: 'user' },
  ];

  let phase = $state('loading'); // loading | setup | login | change (mot de passe provisoire) | app | error
  let user = $state('');
  let bootError = $state('');
  let tab = $state(fromHash());
  let menuOpen = $state(false);

  function fromHash() {
    const id = location.hash.replace(/^#\//, '').split('?')[0];
    return NAV.some((t) => t.id === id) ? id : 'overview';
  }

  /** Navigue vers une page, avec éventuellement des filtres : go('torrents', { status: 'error' }). */
  function go(id, params = {}) {
    const q = new URLSearchParams(params).toString();
    location.hash = `#/${id}${q ? `?${q}` : ''}`;
    menuOpen = false;
  }

  async function boot() {
    phase = 'loading';
    try {
      const h = await api('GET', '/health');
      if (h.setup_required) {
        phase = 'setup';
        return;
      }
      try {
        await enter();
      } catch {
        phase = 'login';
      }
    } catch (e) {
      bootError = e.message;
      phase = 'error';
    }
  }

  /** Session ouverte : écran de changement de mot de passe si celui du compte est provisoire, sinon l'application. */
  async function enter(fallbackUser = '') {
    let me = { username: fallbackUser, must_change_password: false };
    try {
      me = await api('GET', '/me');
    } catch (e) {
      if (!fallbackUser) throw e;
    }
    user = me.username;
    phase = me.must_change_password ? 'change' : 'app';
  }

  async function logout() {
    try {
      await api('POST', '/logout', {});
    } catch {
      /* déjà déconnecté */
    }
    user = '';
    phase = 'login';
  }

  // Le canal temps réel n'existe que tant que l'on est connecté.
  $effect(() => {
    if (phase === 'app') startLive();
    else stopLive();
  });

  const online = $derived(live.stats?.nodes_online ?? 0);
  const alerts = $derived((live.stats?.torrents_error ?? 0) + (live.stats?.nodes_offline ?? 0));
  const THEMES = [
    { id: 'system', icon: 'monitor', label: tr('Thème du système') },
    { id: 'light', icon: 'sun', label: tr('Thème clair') },
    { id: 'dark', icon: 'moon', label: tr('Thème sombre') },
  ];

  onMount(() => {
    // Session expirée pendant l'utilisation : retour à la connexion.
    setUnauthorizedHandler(() => {
      user = '';
      phase = 'login';
    });
    const onHash = () => (tab = fromHash());
    window.addEventListener('hashchange', onHash);
    boot();
    return () => window.removeEventListener('hashchange', onHash);
  });
</script>

{#if phase === 'loading'}
  <div class="center faint">{tr('Chargement…')}</div>
{:else if phase === 'error'}
  <div class="center">
    <div class="card card-pad stack err">
      <h2>{tr('Serveur injoignable')}</h2>
      <p class="muted small">{bootError}</p>
      <Button variant="primary" onclick={boot}>{tr('Réessayer')}</Button>
    </div>
  </div>
{:else if phase === 'setup' || phase === 'login'}
  <Login setupRequired={phase === 'setup'} onsuccess={(u) => enter(u)} />
{:else if phase === 'change'}
  <ForcePassword {user} ondone={() => (phase = 'app')} onlogout={logout} />
{:else}
  <div class="shell">
    <header class="topbar">
      <Button variant="ghost" size="icon" icon="menu" label={tr('Menu')} onclick={() => (menuOpen = !menuOpen)} aria-expanded={menuOpen} />
      <Logo size={22} />
      <strong>{tr('nxtTorrent')}</strong>
    </header>

    {#if menuOpen}<button type="button" class="scrim" aria-label={tr('Fermer le menu')} onclick={() => (menuOpen = false)}></button>{/if}

    <aside class="side" class:open={menuOpen}>
      <div class="brand">
        <Logo size={26} />
        <strong>{tr('nxtTorrent')}</strong>
      </div>

      <nav aria-label={tr('Sections')}>
        {#each NAV as n (n.id)}
          <button type="button" class="nav" aria-current={tab === n.id ? 'page' : undefined} onclick={() => go(n.id)}>
            <Icon name={n.icon} size={16} />
            <span class="grow">{n.label}</span>
            {#if n.id === 'nodes' && live.stats}
              <span class="count tnum" title={tr('{online} en ligne', { online })}>{online}/{num(live.stats.nodes_online + live.stats.nodes_offline + live.stats.nodes_pending)}</span>
            {:else if n.id === 'torrents' && live.stats}
              <span class="count tnum">{num(live.stats.torrents_total)}</span>
            {:else if n.id === 'overview' && alerts > 0}
              <span class="dot" title={tr("{alerts} point(s) d'attention", { alerts })}></span>
            {/if}
          </button>
        {/each}
      </nav>

      <div class="foot">
        <div class="conn" class:ok={live.connected} title={live.connected ? tr('Mises à jour en direct') : tr('Actualisation toutes les 5 s')}>
          <Icon name={live.connected ? 'wifi' : 'wifioff'} size={14} />
          {live.connected ? tr('En direct') : tr('Actualisation périodique')}
        </div>
        <div class="langs"><LangPicker /></div>
        <div class="themes" role="radiogroup" aria-label={tr('Thème')}>
          {#each THEMES as t (t.id)}
            <button type="button" role="radio" aria-checked={theme.mode === t.id} title={t.label} aria-label={t.label} class:on={theme.mode === t.id} onclick={() => setTheme(t.id)}>
              <Icon name={t.icon} size={14} />
            </button>
          {/each}
        </div>
        <div class="who">
          <span class="avatar" aria-hidden="true">{user.slice(0, 1).toUpperCase()}</span>
          <span class="grow truncate small">{user}</span>
          <Button variant="ghost" size="icon" icon="logout" label={tr('Se déconnecter')} onclick={logout} />
        </div>
      </div>
    </aside>

    <main>
      <div class="page">
        {#if tab === 'overview'}<Overview {go} />
        {:else if tab === 'torrents'}<Torrents />
        {:else if tab === 'nodes'}<Nodes {go} />
        {:else if tab === 'stats'}<Stats {go} />
        {:else if tab === 'pairing'}<Pairing />
        {:else if tab === 'users'}<Users />
        {:else if tab === 'account'}<Account />
        {:else if tab === 'alerts'}<Alerts />
        {:else if tab === 'crossseed'}<CrossSeed />
        {:else}<Audit />{/if}
      </div>
    </main>
  </div>
{/if}

<Toaster />

<style>
  .center { min-height: 100vh; display: grid; place-items: center; padding: 1.5rem; text-align: center; }
  .err { width: min(380px, 100%); }

  .shell { display: grid; grid-template-columns: var(--sidebar-w) minmax(0, 1fr); min-height: 100vh; }

  .side {
    position: sticky; top: 0; height: 100vh; display: flex; flex-direction: column;
    background: var(--surface); border-right: 1px solid var(--border); padding: 0.85rem 0.65rem;
  }
  .brand { display: flex; align-items: center; gap: 0.6rem; padding: 0.25rem 0.55rem 1rem; font-size: 0.9375rem; letter-spacing: -0.01em; }
  .langs { display: flex; }
  .langs :global(.sel) { width: 100%; }
  nav { display: flex; flex-direction: column; gap: 2px; }
  .nav {
    appearance: none; border: 0; background: transparent; color: var(--text-2); text-align: left;
    display: flex; align-items: center; gap: 0.65rem; height: 2.15rem; padding: 0 0.6rem; border-radius: 8px;
    font-size: 0.8125rem; font-weight: 550; cursor: pointer;
  }
  .nav:hover { background: var(--surface-2); color: var(--text); }
  .nav[aria-current='page'] { background: var(--surface-3); color: var(--text); }
  .count { font-size: 0.6875rem; color: var(--text-3); font-weight: 500; }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--warn); }

  .foot { margin-top: auto; display: flex; flex-direction: column; gap: 0.7rem; padding-top: 0.8rem; border-top: 1px solid var(--border); }
  .conn { display: flex; align-items: center; gap: 0.45rem; font-size: 0.6875rem; color: var(--text-3); padding: 0 0.4rem; }
  .conn.ok { color: var(--ok); }
  .themes { display: grid; grid-template-columns: repeat(3, 1fr); gap: 2px; padding: 2px; background: var(--surface-2); border: 1px solid var(--border); border-radius: 8px; }
  .themes button {
    appearance: none; border: 0; background: transparent; color: var(--text-3); height: 1.6rem; border-radius: 6px;
    display: grid; place-items: center; cursor: pointer;
  }
  .themes button:hover { color: var(--text); }
  .themes button.on { background: var(--surface); color: var(--text); box-shadow: 0 0 0 1px var(--border); }
  .who { display: flex; align-items: center; gap: 0.55rem; padding: 0 0 0 0.3rem; }
  .avatar { width: 1.6rem; height: 1.6rem; border-radius: 50%; background: var(--primary); color: var(--primary-fg); display: grid; place-items: center; font-size: 0.75rem; font-weight: 650; flex: none; }

  main { min-width: 0; }
  /* Toute la largeur de l'écran (plafond très large pour les écrans ultra-larges). */
  .page { max-width: 2400px; margin: 0 auto; padding: 1.5rem clamp(1rem, 2.2vw, 2.5rem) 4rem; }

  .topbar, .scrim { display: none; }

  @media (max-width: 860px) {
    .shell { display: block; }
    .topbar {
      display: flex; align-items: center; gap: 0.6rem; position: sticky; top: 0; z-index: 20; height: 3.25rem; padding: 0 0.75rem;
      background: color-mix(in srgb, var(--surface) 92%, transparent); backdrop-filter: blur(8px); border-bottom: 1px solid var(--border);
    }
    .side { position: fixed; z-index: 40; inset: 0 auto 0 0; width: min(280px, 82vw); transform: translateX(-102%); transition: transform 0.2s ease; }
    .side.open { transform: none; box-shadow: var(--shadow-lg); }
    .scrim { display: block; position: fixed; inset: 0; z-index: 30; border: 0; background: rgb(8 8 12 / 50%); }
    .page { padding: 1.1rem 1rem 3rem; }
  }
</style>

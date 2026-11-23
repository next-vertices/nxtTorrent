import { mount } from 'svelte';
import './app.css';
import { applyTheme } from './lib/theme.svelte.js';
import App from './App.svelte';
import { initLang } from './lib/lang-loader.js';

applyTheme();
// La langue est chargée avant l'affichage : pas de flash en français pour un navigateur anglophone.
initLang().finally(() => mount(App, { target: document.getElementById('app') }));

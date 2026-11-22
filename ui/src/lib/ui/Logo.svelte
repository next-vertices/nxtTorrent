<script>
  /**
   * Logo de l'application en SVG « en ligne » : il prend la couleur du texte (`currentColor`), donc il suit le thème choisi dans
   * l'application (clair, sombre ou système). Le fichier `favicon.svg`, lui, ne peut suivre que le thème du SYSTÈME : il sert à
   * l'onglet du navigateur ; ici, un `<img>` deviendrait invisible si l'application est en sombre sur un système clair.
   * Proportions du dessin : 100 x 106.
   */
  let { size = 24, class: cls = '' } = $props();
</script>

<svg class={cls} viewBox="0 0 100 106" width={Math.round((size * 100) / 106)} height={size} aria-hidden="true" focusable="false">
  <g stroke="currentColor" stroke-width="7.5" stroke-linecap="round" stroke-linejoin="round" fill="none">
    <path d="M20.36 31.00L39.61 19.85" stroke-dasharray="7.00 8.25" />
    <path d="M60.39 19.85L79.64 31.00" stroke-dasharray="7.00 8.25" />
    <path d="M79.64 31.00L62.98 40.64" stroke-dasharray="7.00 5.25" />
    <path d="M37.02 40.64L20.36 31.00" stroke-dasharray="7.00 5.25" />
    <path d="M50 53L50.00 13.84" />
    <path d="M16.18 38.26L32.84 47.90" stroke-dasharray="7.00 5.25" />
    <path d="M45.82 70.42L45.82 89.74" stroke-dasharray="7.00 5.32" />
    <path d="M45.82 89.74L26.57 78.59" stroke-dasharray="7.00 8.25" />
    <path d="M16.18 60.58L16.18 38.26" stroke-dasharray="7.00 8.32" />
    <path d="M50 53L16.18 72.58" />
    <path d="M83.82 38.26L67.16 47.90" stroke-dasharray="7.00 5.25" />
    <path d="M54.18 70.42L54.18 89.74" stroke-dasharray="7.00 5.32" />
    <path d="M54.18 89.74L73.43 78.59" stroke-dasharray="7.00 8.25" />
    <path d="M83.82 60.58L83.82 38.26" stroke-dasharray="7.00 8.32" />
    <path d="M50 53L83.82 72.58" />
  </g>
  <g fill="currentColor">
    <circle cx="50.00" cy="13.84" r="9.5" />
    <circle cx="16.18" cy="72.58" r="9.5" />
    <circle cx="83.82" cy="72.58" r="9.5" />
    <circle cx="50" cy="53" r="12" />
  </g>
</svg>

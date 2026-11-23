<script>
  // Icônes : tracés 24×24 au trait, dessinés pour ce projet.
  const PATHS = {
    home: 'M3 11l9-8 9 8M5 9.5V21h14V9.5M10 21v-6h4v6',
    folder: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
    server: 'M4 3h16a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zM4 13h16a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-6a1 1 0 0 1 1-1zM7 7.5h.01M7 17.5h.01',
    download: 'M12 3v12M7 10.5l5 5 5-5M4 21h16',
    upload: 'M12 16V4M7 8.5l5-5 5 5M4 21h16',
    activity: 'M3 12h4l3-8 4 16 3-8h4',
    shield: 'M12 3l8 3v6c0 5-3.4 8.2-8 9-4.6-.8-8-4-8-9V6z',
    list: 'M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01',
    key: 'M21 2l-2 2m-7.6 7.6a5.5 5.5 0 1 1-7.8 7.8 5.5 5.5 0 0 1 7.8-7.8zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4',
    plus: 'M12 5v14M5 12h14',
    minus: 'M5 12h14',
    search: 'M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16zM21 21l-4.3-4.3',
    x: 'M18 6L6 18M6 6l12 12',
    check: 'M20 6L9 17l-5-5',
    'chevron-down': 'M6 9l6 6 6-6',
    'chevron-up': 'M18 15l-6-6-6 6',
    'chevron-left': 'M15 18l-6-6 6-6',
    'chevron-right': 'M9 18l6-6-6-6',
    'chevrons-left': 'M11 17l-5-5 5-5M18 17l-5-5 5-5',
    'chevrons-right': 'M13 17l5-5-5-5M6 17l5-5-5-5',
    more: 'M12 12h.01M19 12h.01M5 12h.01',
    pause: 'M7 4h3v16H7zM14 4h3v16h-3z',
    play: 'M7 4l13 8-13 8z',
    trash: 'M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14M10 11v5M14 11v5',
    copy: 'M9 9h11v11H9zM5 15H4V4h11v1',
    sun: 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4',
    moon: 'M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z',
    monitor: 'M3 4h18v12H3zM8 20h8M12 16v4',
    logout: 'M9 21H5V3h4M16 17l5-5-5-5M21 12H9',
    alert: 'M12 3l10 18H2zM12 10v5M12 18h.01',
    refresh: 'M21 12a9 9 0 1 1-3-6.7L21 8M21 3v5h-5',
    file: 'M14 3H6v18h12V7zM14 3v4h4',
    link: 'M10 14a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-1 1M14 10a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l1-1',
    disk: 'M3 13h18M5.5 5h13L21 13v6H3v-6zM7 16.5h.01',
    edit: 'M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z',
    menu: 'M4 6h16M4 12h16M4 18h16',
    inbox: 'M22 12h-6l-2 3h-4l-2-3H2M5.5 5h13L22 12v7H2v-7z',
    clock: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2',
    sort: 'M7 15l5 5 5-5M7 9l5-5 5 5',
    lock: 'M5 11h14v10H5zM8 11V7a4 4 0 0 1 8 0v4',
    layers: 'M12 3l9 5-9 5-9-5zM3 13l9 5 9-5',
    wifi: 'M2 9a15 15 0 0 1 20 0M5.5 12.5a10 10 0 0 1 13 0M9 16a5 5 0 0 1 6 0M12 19.5h.01',
    wifioff: 'M2 2l20 20M8.5 16.4a5 5 0 0 1 3.5-1.4M5 12.6a10 10 0 0 1 4-2.3M2 9a15 15 0 0 1 4.5-3M12 19.5h.01M16.4 11.3A10 10 0 0 1 19 12.6M10.7 5.1A15 15 0 0 1 22 9',
    info: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 11v5M12 8h.01',
    cpu: 'M6 6h12v12H6zM9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4',
    user: 'M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 21a8 8 0 0 1 16 0',
    users: 'M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM2 21a7 7 0 0 1 14 0M16 3.1a4 4 0 0 1 0 7.8M19 14.3A7 7 0 0 1 22 21',
    eye: 'M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z',
  };
  let { name, size = 16, stroke = 1.75, class: cls = '' } = $props();
</script>

<svg
  class={cls}
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <path d={PATHS[name] ?? ''} />
</svg>

<style>
  svg { flex: none; display: block; }
</style>

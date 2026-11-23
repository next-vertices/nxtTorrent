<script>
  // Texte traduit avec un peu de mise en forme : **gras** et `code`. Une phrase reste ainsi traduisible d'un seul bloc
  // (au lieu d'être coupée autour des balises). Rien n'est interprété comme du HTML : les segments sont rendus un à un.
  let { text = '' } = $props();

  const parts = $derived(
    String(text)
      .split(/(\*\*[^*]+\*\*|`[^`]+`)/)
      .filter((p) => p !== '')
      .map((p) => (p.startsWith('**') ? { b: p.slice(2, -2) } : p.startsWith('`') ? { c: p.slice(1, -1) } : { s: p })),
  );
</script>

{#each parts as p, i (i)}{#if p.b !== undefined}<strong>{p.b}</strong>{:else if p.c !== undefined}<code>{p.c}</code>{:else}{p.s}{/if}{/each}

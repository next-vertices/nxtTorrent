<script>
  let { value = $bindable(''), mono = false, rows = 4, ...rest } = $props();
</script>

<textarea bind:value class:mono {rows} {...rest}></textarea>

<style>
  textarea {
    width: 100%; padding: 0.55rem 0.7rem; resize: vertical; min-height: 4.5rem;
    background: var(--surface); color: var(--text);
    border: 1px solid var(--border-strong); border-radius: var(--radius-sm);
    font: inherit; font-size: 0.8125rem; line-height: 1.5; box-shadow: var(--shadow-sm);
  }
  textarea::placeholder { color: var(--text-3); }
  textarea:hover { border-color: var(--ring); }
  textarea:focus { outline: none; border-color: var(--ring); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ring) 22%, transparent); }
  .mono { font-family: var(--mono); font-size: 0.75rem; }
</style>

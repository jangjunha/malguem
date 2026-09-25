<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    title,
    onclose,
    children,
    width = 380,
  }: { title: string; onclose: () => void; children: Snippet; width?: number } = $props();

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onclose();
  }

  // Close on a click that both starts and ends on the backdrop — dragging a
  // slider out of the dialog and releasing over the backdrop must not close it.
  let downOnBackdrop = false;
</script>

<svelte:window {onkeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div
  class="backdrop"
  onpointerdown={(e) => (downOnBackdrop = e.target === e.currentTarget)}
  onclick={(e) => {
    if (downOnBackdrop && e.target === e.currentTarget) onclose();
  }}
  role="presentation"
>
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="modal" role="dialog" aria-modal="true" tabindex="-1" style:width="{width}px">
    <header>
      <h2>{title}</h2>
      <button class="x" onclick={onclose} aria-label="Close">✕</button>
    </header>
    <div class="body">{@render children()}</div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: grid;
    place-items: center;
    z-index: 100;
  }
  .modal {
    background: var(--bg-1);
    border-radius: 12px;
    max-width: 90vw;
    max-height: 90vh;
    overflow-y: auto;
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.4);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 16px;
    border-bottom: 1px solid var(--bg-3);
  }
  h2 { margin: 0; font-size: 15px; }
  .x { background: transparent; color: var(--fg-1); padding: 0 4px; }
  .body { padding: 16px; display: flex; flex-direction: column; gap: 12px; }
</style>

<script lang="ts">
  import { MAX_GAIN } from '../lib/mixer';
  import { store } from '../lib/store.svelte';

  /** Per-user playback volume, remembered per server across calls. */
  let { userId, name, onclose }: { userId: string; name: string; onclose: () => void } = $props();

  const pct = $derived(Math.round(store.peerVolume(userId) * 100));
  let el = $state<HTMLElement | null>(null);

  function onWindowPointer(e: PointerEvent) {
    if (el && !el.contains(e.target as Node)) onclose();
  }
  function onWindowKey(e: KeyboardEvent) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window onpointerdowncapture={onWindowPointer} onkeydown={onWindowKey} />

<div class="pop" bind:this={el} role="dialog" aria-label="{name} volume">
  <div class="head">
    <span class="who">{name}</span>
    <span class="num">{pct}%</span>
  </div>
  <!-- svelte-ignore a11y_autofocus -->
  <input
    type="range"
    min="0"
    max={MAX_GAIN * 100}
    step="5"
    value={pct}
    autofocus
    oninput={(e) => store.setPeerVolume(userId, e.currentTarget.valueAsNumber / 100)}
    aria-label="{name} volume"
  />
  <div class="actions">
    <button class="link" onclick={() => store.setPeerVolume(userId, pct === 0 ? 1 : 0)}>
      {pct === 0 ? 'Unmute for me' : 'Mute for me'}
    </button>
    {#if pct !== 100}
      <button class="link" onclick={() => store.setPeerVolume(userId, 1)}>Reset</button>
    {/if}
  </div>
</div>

<style>
  .pop {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 40;
    width: 200px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--bg-1);
    border: 1px solid var(--bg-3);
    border-radius: var(--radius);
    padding: 10px 12px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.45);
    cursor: default;
    text-align: left;
  }
  .head { display: flex; justify-content: space-between; gap: 8px; font-size: 12px; }
  .who { color: var(--fg-0); font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .num { color: var(--fg-1); font-variant-numeric: tabular-nums; }
  input[type='range'] { width: 100%; padding: 0; border: none; background: transparent; accent-color: var(--accent); }
  .actions { display: flex; justify-content: space-between; }
  .link { background: transparent; color: var(--fg-1); font-size: 12px; padding: 0; }
  .link:hover { color: var(--fg-0); }
</style>

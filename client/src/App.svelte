<script lang="ts">
  import { onMount } from 'svelte';
  import Main from './components/Main.svelte';
  import Onboarding from './components/Onboarding.svelte';
  import SettingsModal from './components/SettingsModal.svelte';
  import { Hotkeys } from './lib/hotkeys';
  import { store } from './lib/store.svelte';

  const hotkeys = new Hotkeys((action) => {
    if (action === 'toggleMute') store.toggleMic();
    else store.toggleDeafen();
  });

  onMount(() => {
    void store.bootstrap();
    return () => void hotkeys.dispose();
  });

  // Re-register whenever a binding or the global toggle changes. OS-wide only
  // while in a call: outside one, Ctrl+Shift+M/D stay with other apps.
  const inCall = $derived(store.call !== null);
  $effect(() => {
    const binds = $state.snapshot(store.keybinds);
    void hotkeys
      .sync({ ...binds, global: binds.global && inCall })
      .then((status) => (store.hotkeyStatus = status));
  });
</script>

<svelte:window onkeydown={(e) => hotkeys.onKeydown(e)} />

{#if store.phase === 'loading'}
  <div class="center">connecting…</div>
{:else if store.phase === 'onboarding'}
  <Onboarding />
{:else}
  <Main />
{/if}

{#if store.settingsOpen}
  <SettingsModal />
{/if}

{#if store.error}
  <div class="toast" role="alert">
    {store.error}
    <button onclick={() => (store.error = null)}>✕</button>
  </div>
{/if}

<style>
  .center {
    height: 100%;
    display: grid;
    place-items: center;
    color: var(--fg-1);
  }
  .toast {
    position: fixed;
    bottom: 16px;
    left: 50%;
    transform: translateX(-50%);
    background: var(--danger);
    color: #0d1117;
    padding: 8px 12px;
    border-radius: var(--radius);
    display: flex;
    gap: 8px;
    align-items: center;
    max-width: 80vw;
  }
  .toast button { background: transparent; color: inherit; padding: 0 4px; }
</style>

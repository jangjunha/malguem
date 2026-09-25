<script lang="ts">
  import { store } from '../lib/store.svelte';

  /**
   * "Connection lost — reconnecting in 4s [Retry now]" for one server. Calls
   * are peer-to-peer, so people already connected keep hearing each other;
   * the banner says so when you're in a call on that server.
   */
  let { serverId }: { serverId: string } = $props();

  const st = $derived(store.socketStatus[serverId]);
  const reconnecting = $derived(st?.state === 'reconnecting');
  const inCallHere = $derived(store.call?.serverId === serverId);

  let now = $state(Date.now());
  $effect(() => {
    if (!reconnecting) return;
    const t = setInterval(() => (now = Date.now()), 500);
    return () => clearInterval(t);
  });
  const secs = $derived(st?.retryAt ? Math.max(0, Math.ceil((st.retryAt - now) / 1000)) : 0);
</script>

{#if reconnecting}
  <div class="banner" role="status">
    <span class="dot"></span>
    <span class="text">
      Connection to the server lost —
      {secs > 0 ? `reconnecting in ${secs}s` : 'reconnecting…'}
      {#if (st?.attempts ?? 0) > 1}<span class="dim">(attempt {st?.attempts})</span>{/if}
      {#if inCallHere}<span class="dim">· voice with people already connected continues</span>{/if}
    </span>
    <button onclick={() => store.retryConnection(serverId)}>Retry now</button>
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 16px;
    background: color-mix(in srgb, #d29922 22%, var(--bg-1));
    border-bottom: 1px solid color-mix(in srgb, #d29922 45%, transparent);
    font-size: 12.5px;
  }
  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #d29922;
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse { 50% { opacity: 0.3; } }
  .text { flex: 1; min-width: 0; }
  .dim { color: var(--fg-1); }
  button { padding: 3px 10px; font-size: 12px; background: var(--bg-3); }
</style>

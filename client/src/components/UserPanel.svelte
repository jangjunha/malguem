<script lang="ts">
  import Icon from './Icon.svelte';
  import { formatAccelerator } from '../lib/keybinds';
  import { store } from '../lib/store.svelte';

  /**
   * Always-visible strip at the bottom of the sidebar (Discord's user panel):
   * the voice connection, your name, and mic / deafen / settings — so mute is
   * one click away from any channel, in a call or not.
   */
  const call = $derived(store.call);
  const selfName = $derived.by(() => {
    const sid = call?.serverId ?? store.activeServerId;
    return store.servers.find((s) => s.id === sid)?.userName ?? '';
  });
  const channelName = $derived.by(() => {
    if (!call) return '';
    const sp = store.spacesOf(call.serverId).find((s) => s.id === call.spaceId);
    const ch = sp?.channels.find((c) => c.id === call.channelId);
    return `${ch ? '#' + ch.name : 'call'}${sp ? ' / ' + sp.name : ''}`;
  });
  const speaking = $derived(!!call && store.speaking[call.selfId] === true);

  function goToCall() {
    if (call) void store.selectChannel(call.serverId, call.spaceId, call.channelId);
  }
</script>

<div class="user-panel">
  {#if call}
    <div class="voice">
      <button class="voice-info" onclick={goToCall} title="Go to the call">
        <span class="status">Voice connected</span>
        <span class="where">{channelName}</span>
      </button>
      <button
        class="icon-btn"
        title={call.broadcasting ? 'Stop sharing' : 'Share screen'}
        class:on={call.broadcasting}
        disabled={call.broadcastStarting}
        onclick={() => store.toggleBroadcast()}
      >
        <Icon name={call.broadcasting ? 'screen-off' : 'screen'} />
      </button>
      <button class="icon-btn hang" title="Disconnect" onclick={() => store.leaveCall()}>
        <Icon name="hangup" />
      </button>
    </div>
  {/if}
  <div class="me">
    <span class="avatar" class:speaking>{selfName.slice(0, 1).toUpperCase()}</span>
    <span class="name" title={selfName}>{selfName}</span>
    <button
      class="icon-btn"
      class:muted={store.micMuted}
      aria-pressed={store.micMuted}
      title="{store.micMuted ? 'Unmute' : 'Mute'} ({formatAccelerator(store.keybinds.toggleMute)})"
      onclick={() => store.toggleMic()}
    >
      <Icon name={store.micMuted ? 'mic-off' : 'mic'} />
    </button>
    <button
      class="icon-btn"
      class:muted={store.deafened}
      aria-pressed={store.deafened}
      title="{store.deafened ? 'Undeafen' : 'Deafen'} ({formatAccelerator(store.keybinds.toggleDeafen)})"
      onclick={() => store.toggleDeafen()}
    >
      <Icon name={store.deafened ? 'headphones-off' : 'headphones'} />
    </button>
    <button class="icon-btn" title="Voice & keybind settings" onclick={() => (store.settingsOpen = 'voice')}>
      <Icon name="gear" />
    </button>
  </div>
</div>

<style>
  .user-panel {
    background: var(--bg-0);
    border-top: 1px solid var(--bg-3);
    padding: 6px 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .voice {
    display: flex;
    align-items: center;
    gap: 2px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--bg-3);
  }
  .voice-info {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    background: transparent;
    padding: 2px 4px;
    text-align: left;
  }
  .status { color: var(--ok); font-weight: 600; font-size: 12.5px; }
  .where {
    color: var(--fg-1);
    font-size: 11.5px;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .voice-info:hover .where { text-decoration: underline; }

  .me { display: flex; align-items: center; gap: 2px; }
  .avatar {
    flex: none;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: var(--bg-3);
    display: grid;
    place-items: center;
    font-weight: 700;
    font-size: 12px;
    margin-right: 6px;
    box-shadow: 0 0 0 2px transparent;
    transition: box-shadow 80ms;
  }
  .avatar.speaking { box-shadow: 0 0 0 2px var(--ok); }
  .name {
    flex: 1;
    min-width: 0;
    font-weight: 600;
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .icon-btn {
    flex: none;
    background: transparent;
    color: var(--fg-1);
    padding: 6px;
    border-radius: 6px;
  }
  .icon-btn:hover { background: var(--bg-3); color: var(--fg-0); filter: none; }
  .icon-btn.muted { color: var(--danger); }
  .icon-btn.on { color: var(--ok); }
  .icon-btn.hang:hover { color: var(--danger); }
</style>

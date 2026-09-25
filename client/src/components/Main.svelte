<script lang="ts">
  import ChannelView from './ChannelView.svelte';
  import Modal from './Modal.svelte';
  import ServerForm from './ServerForm.svelte';
  import UserPanel from './UserPanel.svelte';
  import VolumePopover from './VolumePopover.svelte';
  import Icon from './Icon.svelte';
  import { copyText } from '../lib/clipboard';
  import { store } from '../lib/store.svelte';

  // Native prompt()/alert()/confirm() are no-ops in some WebViews (e.g. macOS
  // WKWebView), so every interaction goes through an in-app modal instead.
  type ModalState =
    | null
    | { kind: 'addServer' }
    | { kind: 'newSpace'; serverId: string }
    | { kind: 'join'; serverId: string }
    | { kind: 'invite'; token: string }
    | { kind: 'backup'; data: string }
    | { kind: 'info'; serverId: string }
    | { kind: 'kick'; serverId: string; spaceId: string; userId: string; name: string }
    | { kind: 'removeServer'; serverId: string; label: string };

  let modal = $state<ModalState>(null);
  let textInput = $state('');
  let copied = $state(false);
  let busy = $state(false);

  /** Call participant whose volume popover is open in the sidebar. */
  let sideVolumeFor = $state<string | null>(null);

  let newChannelName = $state('');
  let addingChannel = $state(false);

  const serverId = $derived(store.activeServerId);
  const space = $derived(store.activeSpace);
  const isOwner = $derived(
    space !== null && serverId !== null && space.owner_id === store.userIdOf(serverId),
  );

  function serverLabel(serverUrl: string): string {
    try {
      return new URL(serverUrl).host;
    } catch {
      return serverUrl;
    }
  }

  function open(state: ModalState) {
    textInput = '';
    copied = false;
    modal = state;
  }
  function close() {
    modal = null;
  }

  async function submitNewSpace(e: SubmitEvent) {
    e.preventDefault();
    if (modal?.kind !== 'newSpace') return;
    const name = textInput.trim();
    if (!name) return;
    busy = true;
    try {
      await store.createSpace(modal.serverId, name);
      close();
    } catch (err) {
      store.error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function submitJoin(e: SubmitEvent) {
    e.preventDefault();
    if (modal?.kind !== 'join') return;
    const token = textInput.trim();
    if (!token) return;
    busy = true;
    try {
      await store.acceptInvite(modal.serverId, token);
      close();
    } catch (err) {
      store.error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function invite() {
    if (!space || !serverId) return;
    busy = true;
    try {
      const token = await store.createInvite(serverId, space.id);
      open({ kind: 'invite', token });
      copied = await copyText(token);
    } catch (err) {
      store.error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  function backupIdentity(sid: string) {
    open({ kind: 'backup', data: store.exportIdentity(sid) });
  }

  async function copyCurrent(text: string) {
    copied = await copyText(text);
  }

  async function confirmKick() {
    if (modal?.kind !== 'kick') return;
    busy = true;
    try {
      await store.removeMember(modal.serverId, modal.spaceId, modal.userId);
      close();
    } catch (err) {
      store.error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function confirmRemoveServer() {
    if (modal?.kind !== 'removeServer') return;
    busy = true;
    try {
      await store.removeServer(modal.serverId);
      close();
    } catch (err) {
      store.error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function addChannel(e: SubmitEvent) {
    e.preventDefault();
    if (!space || !serverId || !newChannelName.trim()) return;
    await store.createChannel(serverId, space.id, newChannelName.trim());
    newChannelName = '';
    addingChannel = false;
  }
</script>

<div class="layout">
  <aside class="sidebar">
    <div class="servers">
      {#each store.servers as srv (srv.id)}
        <section class="server">
          <header class="server-head">
            <span class="dot {srv.status}" title={srv.error ?? srv.status}></span>
            <span class="server-name" title={srv.serverUrl}>{serverLabel(srv.serverUrl)}</span>
            <button class="icon" title="Account info" onclick={() => open({ kind: 'info', serverId: srv.id })}>ⓘ</button>
            <button class="icon" title="Back up identity" onclick={() => backupIdentity(srv.id)}>🔑</button>
            <button
              class="icon"
              title="Disconnect & forget server"
              onclick={() => open({ kind: 'removeServer', serverId: srv.id, label: serverLabel(srv.serverUrl) })}
            >✕</button>
          </header>

          {#if srv.status === 'offline'}
            <p class="server-error">{srv.error ?? 'disconnected'}</p>
          {/if}

          {#each store.spacesOf(srv.id) as sp (sp.id)}
            {@const active = srv.id === serverId && sp.id === store.activeSpaceId}
            <button
              class="space"
              class:active={active}
              onclick={() => store.selectChannel(srv.id, sp.id, sp.channels[0]?.id ?? null)}
            >
              {sp.name}
            </button>
            {#if active}
              <div class="channels">
                {#each sp.channels as c (c.id)}
                  {@const inCall = store.call?.serverId === srv.id && store.call.channelId === c.id}
                  <button
                    class="channel"
                    class:active={c.id === store.activeChannelId}
                    onclick={() => store.selectChannel(srv.id, sp.id, c.id)}
                  >
                    # {c.name}
                    {#if inCall}<span class="live" title="You're in this channel's call">●</span>{/if}
                  </button>
                  {#if inCall && store.call}
                    {@const call = store.call}
                    <!-- Who's in the call, Discord-style under the channel. -->
                    <ul class="voice-users">
                      {#each call.participants as p (p)}
                        {@const self = p === call.selfId}
                        {@const muted = self ? store.micMuted : call.peerStates[p]?.muted}
                        {@const deaf = self ? store.deafened : call.peerStates[p]?.deafened}
                        <li class="voice-user-wrap">
                          <button
                            class="voice-user"
                            disabled={self}
                            title={self ? 'You' : 'Click to adjust volume'}
                            onclick={() => (sideVolumeFor = sideVolumeFor === p ? null : p)}
                          >
                            <span class="mini-avatar" class:speaking={store.speaking[p]}>
                              {store.memberName(srv.id, sp.id, p).slice(0, 1).toUpperCase()}
                            </span>
                            <span class="vu-name">{store.memberName(srv.id, sp.id, p)}</span>
                            {#if call.broadcasting && self}<span class="tag">LIVE</span>{/if}
                            {#if muted}<span class="vu-state"><Icon name="mic-off" size={13} /></span>{/if}
                            {#if deaf}<span class="vu-state"><Icon name="headphones-off" size={13} /></span>{/if}
                          </button>
                          {#if sideVolumeFor === p && !self}
                            <VolumePopover
                              userId={p}
                              name={store.memberName(srv.id, sp.id, p)}
                              onclose={() => (sideVolumeFor = null)}
                            />
                          {/if}
                        </li>
                      {/each}
                    </ul>
                  {/if}
                {/each}
                {#if isOwner}
                  {#if addingChannel}
                    <form onsubmit={addChannel}>
                      <input bind:value={newChannelName} placeholder="channel name" maxlength="64" />
                    </form>
                  {:else}
                    <button class="channel dim" onclick={() => (addingChannel = true)}>+ channel</button>
                  {/if}
                {/if}
              </div>
            {/if}
          {/each}

          <div class="server-actions">
            <button class="link" onclick={() => open({ kind: 'newSpace', serverId: srv.id })}>+ space</button>
            <button class="link" onclick={() => open({ kind: 'join', serverId: srv.id })}>join</button>
          </div>
        </section>
      {/each}

      <button class="add-server" onclick={() => open({ kind: 'addServer' })}>+ Add server</button>
    </div>

    {#if space && serverId}
      <div class="members">
        <h3>Members · {space.name}</h3>
        {#each store.membersOf(serverId, space.id) as m (m.user_id)}
          <div class="member">
            <span class:me={m.user_id === store.userIdOf(serverId)}>{m.name}</span>
            {#if m.role === 'owner'}<span class="badge">owner</span>{/if}
            {#if isOwner && m.user_id !== store.userIdOf(serverId)}
              <button
                class="kick"
                title="Remove member"
                onclick={() => open({ kind: 'kick', serverId, spaceId: space.id, userId: m.user_id, name: m.name })}
              >✕</button>
            {/if}
          </div>
        {/each}
        {#if isOwner}
          <button class="invite" onclick={invite}>Create invite</button>
        {/if}
      </div>
    {/if}
    <UserPanel />
  </aside>

  <main>
    {#if serverId && store.activeChannelId && space}
      <ChannelView {serverId} channelId={store.activeChannelId} {space} />
    {:else}
      <div class="empty">No channel selected</div>
    {/if}
  </main>
</div>

{#if modal?.kind === 'addServer'}
  <Modal title="Add server" onclose={close}>
    <ServerForm onDone={close} />
  </Modal>
{:else if modal?.kind === 'newSpace'}
  <Modal title="Create space" onclose={close}>
    <form onsubmit={submitNewSpace}>
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={textInput} placeholder="Space name" maxlength="64" autofocus />
      <div class="actions">
        <button type="button" onclick={close}>Cancel</button>
        <button class="primary" disabled={busy || !textInput.trim()}>Create</button>
      </div>
    </form>
  </Modal>
{:else if modal?.kind === 'join'}
  <Modal title="Join with invite" onclose={close}>
    <form onsubmit={submitJoin}>
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={textInput} placeholder="Paste invite token" autofocus />
      <div class="actions">
        <button type="button" onclick={close}>Cancel</button>
        <button class="primary" disabled={busy || !textInput.trim()}>Join</button>
      </div>
    </form>
  </Modal>
{:else if modal?.kind === 'invite'}
  {@const token = modal.token}
  <Modal title="Invite token" onclose={close}>
    <p class="note">Share this token with a friend. Valid for 7 days.</p>
    <input class="reveal" readonly value={token} onfocus={(e) => e.currentTarget.select()} />
    <div class="actions">
      <button class="primary" onclick={() => copyCurrent(token)}>
        {copied ? 'Copied!' : 'Copy'}
      </button>
    </div>
  </Modal>
{:else if modal?.kind === 'info'}
  {@const sid = modal.serverId}
  <Modal title="Account info" onclose={close}>
    <p class="note">
      Your public identity on this server. The <b>signing public key</b> is what
      other members pin to verify your messages and calls — it's safe to share.
    </p>
    <label class="kv">
      User ID
      <input class="reveal" readonly value={store.userIdOf(sid) ?? ''} onfocus={(e) => e.currentTarget.select()} />
    </label>
    <label class="kv">
      Signing public key
      <input class="reveal" readonly value={store.selfSignPub(sid)} onfocus={(e) => e.currentTarget.select()} />
    </label>
    <label class="kv">
      Fingerprint
      <input class="reveal" readonly value={store.selfFingerprint(sid)} onfocus={(e) => e.currentTarget.select()} />
    </label>
    <div class="actions">
      <button class="primary" onclick={() => copyCurrent(store.selfSignPub(sid))}>
        {copied ? 'Copied!' : 'Copy key'}
      </button>
    </div>
  </Modal>
{:else if modal?.kind === 'backup'}
  {@const data = modal.data}
  <Modal title="Identity backup" onclose={close}>
    <p class="note">
      This is the only way to move your account to another device. Store it
      somewhere safe and private — anyone with it can read your messages.
    </p>
    <textarea class="reveal" readonly rows="4" onfocus={(e) => e.currentTarget.select()}>{data}</textarea>
    <div class="actions">
      <button class="primary" onclick={() => copyCurrent(data)}>
        {copied ? 'Copied!' : 'Copy'}
      </button>
    </div>
  </Modal>
{:else if modal?.kind === 'kick'}
  {@const name = modal.name}
  <Modal title="Remove member" onclose={close}>
    <p class="note">Remove <b>{name}</b> from this space? The space key will be rotated so they can't read new messages.</p>
    <div class="actions">
      <button type="button" onclick={close}>Cancel</button>
      <button class="danger" disabled={busy} onclick={confirmKick}>Remove</button>
    </div>
  </Modal>
{:else if modal?.kind === 'removeServer'}
  {@const label = modal.label}
  <Modal title="Forget server" onclose={close}>
    <p class="note">
      Disconnect from <b>{label}</b> and remove its identity key from this
      device? Back up the identity first if you want to reconnect later.
    </p>
    <div class="actions">
      <button type="button" onclick={close}>Cancel</button>
      <button class="danger" disabled={busy} onclick={confirmRemoveServer}>Forget</button>
    </div>
  </Modal>
{/if}

<style>
  .layout {
    display: grid;
    grid-template-columns: 240px 1fr;
    grid-template-rows: minmax(0, 1fr);
    height: 100%;
  }

  .sidebar {
    background: var(--bg-1);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  /* Servers and members scroll; the user panel stays pinned at the bottom. */
  .servers {
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
  }

  .server { display: flex; flex-direction: column; gap: 2px; padding-bottom: 6px; }
  .server-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 6px 4px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-1);
  }
  .server-name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .server-head .icon {
    background: transparent;
    color: var(--fg-1);
    padding: 0 3px;
    font-size: 11px;
    opacity: 0.6;
  }
  .server-head .icon:hover { opacity: 1; }
  .dot { width: 7px; height: 7px; border-radius: 50%; flex: none; background: var(--fg-1); }
  .dot.online { background: #3fb950; }
  .dot.connecting { background: #d29922; }
  .dot.offline { background: var(--danger); }
  .server-error { margin: 0 6px; font-size: 11.5px; color: var(--danger); }

  .space {
    background: transparent;
    text-align: left;
    color: var(--fg-0);
    padding: 5px 8px;
    font-weight: 600;
    font-size: 13px;
    border-radius: var(--radius);
  }
  .space.active { background: var(--bg-3); }

  .channels { display: flex; flex-direction: column; gap: 2px; padding: 2px 0 4px 10px; }
  .channel {
    background: transparent;
    text-align: left;
    color: var(--fg-1);
    padding: 4px 8px;
    border-radius: var(--radius);
  }
  .channel.active { background: var(--bg-3); color: var(--fg-0); }
  .channel .live { color: var(--ok); font-size: 9px; margin-left: 4px; vertical-align: middle; }

  .voice-users { list-style: none; margin: 0 0 4px; padding: 0 0 0 14px; display: flex; flex-direction: column; gap: 1px; }
  .voice-user-wrap { position: relative; }
  .voice-user {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    color: var(--fg-1);
    padding: 3px 6px;
    font-size: 12.5px;
    text-align: left;
  }
  .voice-user:disabled { opacity: 1; cursor: default; }
  .voice-user:not(:disabled):hover { background: var(--bg-2); color: var(--fg-0); }
  .mini-avatar {
    flex: none;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--bg-3);
    display: grid;
    place-items: center;
    font-size: 10px;
    font-weight: 700;
    color: var(--fg-0);
    box-shadow: 0 0 0 2px transparent;
  }
  .mini-avatar.speaking { box-shadow: 0 0 0 2px var(--ok); }
  .vu-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .vu-state { color: var(--danger); display: inline-flex; }
  .tag { background: var(--danger); color: #0d1117; font-size: 9px; font-weight: 700; border-radius: 3px; padding: 0 4px; }
  .channel.dim { font-size: 12.5px; }
  .channels input { width: 100%; }

  .server-actions { display: flex; gap: 10px; padding: 2px 8px; }
  .link { background: transparent; color: var(--fg-1); font-size: 12px; padding: 2px 0; }
  .link:hover { color: var(--fg-0); }

  .add-server {
    margin-top: 6px;
    background: var(--bg-2);
    color: var(--fg-1);
    font-size: 12.5px;
    padding: 6px;
  }

  .members {
    padding: 8px 12px;
    border-top: 1px solid var(--bg-3);
    flex: 0 1 auto;
    max-height: 35%;
    overflow-y: auto;
  }
  .members h3 {
    font-size: 11px;
    text-transform: uppercase;
    color: var(--fg-1);
    margin: 4px 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .member { display: flex; align-items: center; gap: 6px; padding: 3px 0; color: var(--fg-1); }
  .member .me { color: var(--fg-0); font-weight: 600; }
  .badge {
    font-size: 10px;
    background: var(--bg-3);
    border-radius: 4px;
    padding: 1px 5px;
    color: var(--fg-1);
  }
  .kick { margin-left: auto; background: transparent; color: var(--fg-1); padding: 0 4px; }
  .invite { margin-top: 8px; width: 100%; font-size: 12.5px; }

  main { background: var(--bg-2); min-width: 0; min-height: 0; overflow: hidden; }
  .empty { color: var(--fg-1); display: grid; place-items: center; height: 100%; }

  /* modal contents */
  form { display: flex; flex-direction: column; gap: 12px; }
  form input { width: 100%; }
  .actions { display: flex; justify-content: flex-end; gap: 8px; }
  .note { margin: 0; color: var(--fg-1); font-size: 13px; line-height: 1.45; }
  .kv {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-1);
  }
  .reveal {
    width: 100%;
    text-transform: none;
    letter-spacing: normal;
    color: var(--fg-0);
    font-family: ui-monospace, monospace;
    font-size: 12px;
    word-break: break-all;
    resize: vertical;
  }
</style>

<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { MAX_GAIN } from '../lib/mixer';

  let showSettings = $state(false);
  /** Stream id currently shown large; null = grid view. */
  let spotlightId = $state<string | null>(null);
  /** Participant id whose volume slider is open; null = none. */
  let volumeFor = $state<string | null>(null);

  const call = $derived(store.call);
  const s = $derived(store.broadcastSettings);

  function toggleSpotlight(id: string) {
    spotlightId = spotlightId === id ? null : id;
  }

  /** Discord-style call shortcuts, active only while a call is up. */
  function onWindowKeydown(e: KeyboardEvent) {
    if (!store.call) return;
    const t = e.target as HTMLElement | null;
    const typing =
      !!t && (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName));
    if (e.key === 'Escape' && volumeFor) {
      volumeFor = null;
      return;
    }
    if (typing) return;
    // `code` is keyboard-layout independent (KeyM stays KeyM under Shift).
    const mod = e.ctrlKey || e.metaKey;
    if (mod && e.shiftKey && e.code === 'KeyM') {
      e.preventDefault();
      store.toggleMic();
    } else if (mod && e.shiftKey && e.code === 'KeyD') {
      e.preventDefault();
      store.toggleDeafen();
    }
  }

  function volumePct(userId: string): number {
    return Math.round(store.peerVolume(userId) * 100);
  }

  function toggleFullscreen(node: HTMLVideoElement) {
    if (document.fullscreenElement) {
      document.exitFullscreen();
    } else {
      node.requestFullscreen?.();
    }
  }

  /** Codecs actually negotiable in this webview (spike: verify HW variants). */
  const availableCodecs: string[] = (() => {
    const caps = typeof RTCRtpReceiver !== 'undefined' ? RTCRtpReceiver.getCapabilities('video') : null;
    const mimes = new Set((caps?.codecs ?? []).map((c) => c.mimeType.split('/')[1]?.toUpperCase()));
    return ['H264', 'VP9', 'AV1', 'H265'].filter((c) => mimes.has(c));
  })();

  function srcObject(node: HTMLMediaElement, stream: MediaStream) {
    node.srcObject = stream;
    // All audible output comes from the AudioMixer; every element here (local
    // preview and remote tiles alike) stays muted so audio isn't double-played.
    node.muted = true;
    return {
      update(next: MediaStream) {
        if (node.srcObject !== next) node.srcObject = next;
      },
    };
  }

  function hasVideo(stream: MediaStream): boolean {
    return stream.getVideoTracks().length > 0;
  }

  function name(userId: string): string {
    return call ? store.memberName(call.serverId, call.spaceId, userId) : userId;
  }

  /** Relay-tree status of a broadcast we watch (experimental relay transport). */
  function relayLine(userId: string): string {
    const r = call?.relay?.incoming[userId];
    if (!r || !r.parent) return '';
    const via = r.parent === userId ? 'direct' : `via ${name(r.parent)}`;
    const reduced = r.rxLayer >= 0 && r.rxLayer < 2 ? ' (reduced fps)' : '';
    return `relay ${via} · ${r.rxFps}fps${reduced}`;
  }

  /** Our own relayed broadcast: who we feed directly and how deep the tree is. */
  function relayOutLine(): string {
    const o = call?.relay?.outgoing;
    if (!o) return '';
    const direct = Object.values(o.tree).filter((p) => p === call?.selfId).length;
    const light = call?.relay?.local.busy ? ' · light mode (game detected)' : '';
    return `relay · ${o.viewers.length} watching · ${direct} fed directly${light}`;
  }

  function statLine(userId: string): string {
    const relay = relayLine(userId);
    if (relay) return relay;
    const st = call?.stats[userId];
    if (!st) return '';
    const parts: string[] = [];
    if (st.rttMs != null) parts.push(`${st.rttMs.toFixed(0)}ms`);
    if (st.outKbps > 0) parts.push(`↑${(st.outKbps / 1000).toFixed(1)}Mb/s`);
    if (st.inKbps > 0) parts.push(`↓${(st.inKbps / 1000).toFixed(1)}Mb/s`);
    if (st.outFps != null) parts.push(`${st.outFps}fps`);
    else if (st.inFps != null) parts.push(`${st.inFps}fps`);
    if (st.jitterBufferMs != null) parts.push(`jb ${st.jitterBufferMs.toFixed(0)}ms`);
    if (st.encoder) parts.push(st.encoder.includes('libvpx') || st.encoder.includes('OpenH264') ? 'sw-enc' : 'hw-enc');
    if (st.qualityLimitation && st.qualityLimitation !== 'none') parts.push(`limited:${st.qualityLimitation}`);
    if (st.transport === 'relay') parts.push('via relay');
    return parts.join(' · ');
  }

  const uploadEstimate = $derived(
    call && call.broadcasting
      ? call.relay?.outgoing
        ? (s.maxBitrateKbps * Math.max(1, Object.values(call.relay.outgoing.tree).filter((p) => p === call.selfId).length)) / 1000
        : (s.maxBitrateKbps * Math.max(call.participants.length - 1, 1)) / 1000
      : 0,
  );
</script>

<svelte:window onkeydown={onWindowKeydown} />

{#if call}
  <section class="panel">
    <div class="row controls">
      <div class="participants">
        {#each call.participants as p (p)}
          {#if p === call.selfId}
            <span class="chip me">{name(p)}</span>
          {:else}
            <div class="chip-wrap">
              <button
                class="chip"
                class:adjusted={volumePct(p) !== 100}
                title="Adjust {name(p)}'s volume"
                onclick={() => (volumeFor = volumeFor === p ? null : p)}
              >
                {name(p)}{#if volumePct(p) !== 100}<span class="vol-badge">{volumePct(p)}%</span>{/if}
              </button>
              {#if volumeFor === p}
                <div class="vol-pop">
                  <input
                    type="range"
                    min="0"
                    max={MAX_GAIN * 100}
                    step="5"
                    value={volumePct(p)}
                    oninput={(e) => store.setPeerVolume(p, e.currentTarget.valueAsNumber / 100)}
                    aria-label="{name(p)} volume"
                  />
                  <span class="vol-num">{volumePct(p)}%</span>
                </div>
              {/if}
            </div>
          {/if}
        {/each}
      </div>
      <div class="buttons">
        <button onclick={() => store.toggleMic()} title="{call.micMuted ? 'Unmute' : 'Mute'} (Ctrl+Shift+M)">
          {call.micMuted ? 'Unmute' : 'Mute'}
        </button>
        <button
          class:active={call.deafened}
          onclick={() => store.toggleDeafen()}
          title="{call.deafened ? 'Undeafen' : 'Deafen'} (Ctrl+Shift+D)"
        >
          {call.deafened ? 'Undeafen' : 'Deafen'}
        </button>
        <button class={call.broadcasting ? 'danger' : 'primary'} onclick={() => store.toggleBroadcast()}>
          {call.broadcasting ? 'Stop sharing' : 'Share screen'}
        </button>
        <button onclick={() => (showSettings = !showSettings)} title="Broadcast settings">⚙</button>
        <button class="danger" onclick={() => store.leaveCall()}>Leave</button>
      </div>
    </div>

    {#if showSettings}
      <div class="row settings">
        <label>
          Codec
          <select bind:value={s.codec} onchange={() => store.applyBroadcastSettings()}>
            <option value="auto">auto</option>
            {#each availableCodecs as c (c)}
              <option value={c}>{c}</option>
            {/each}
          </select>
        </label>
        <label>
          Max bitrate
          <select bind:value={s.maxBitrateKbps} onchange={() => store.applyBroadcastSettings()}>
            <option value={2500}>2.5 Mb/s</option>
            <option value={5000}>5 Mb/s</option>
            <option value={8000}>8 Mb/s</option>
            <option value={12000}>12 Mb/s</option>
            <option value={20000}>20 Mb/s</option>
          </select>
        </label>
        <label>
          Resolution
          <select bind:value={s.height} onchange={() => store.applyBroadcastSettings()}>
            <option value={720}>720p</option>
            <option value={1080}>1080p</option>
            <option value={1440}>1440p</option>
            <option value={0}>native</option>
          </select>
        </label>
        <label>
          FPS
          <select bind:value={s.frameRate} onchange={() => store.applyBroadcastSettings()}>
            <option value={30}>30</option>
            <option value={60}>60</option>
          </select>
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={s.systemAudio} />
          Game/system audio
        </label>
        <label title="Relay: encode once, viewers pass the stream on to each other. Applies from the next share.">
          Transport
          <select bind:value={s.transport} disabled={call.broadcasting}>
            <option value="webrtc">WebRTC mesh</option>
            <option value="relay">Relay tree (experimental)</option>
          </select>
        </label>
        <label title="Upload you can spare to pass others' relayed broadcasts on">
          Relay upload
          <select bind:value={s.relayUploadMbps} onchange={() => store.applyBroadcastSettings()}>
            <option value={0}>none</option>
            <option value={10}>10 Mb/s</option>
            <option value={20}>20 Mb/s</option>
            <option value={50}>50 Mb/s</option>
            <option value={100}>100 Mb/s</option>
          </select>
        </label>
        {#if call.outputDevices.length > 1}
          <label>
            Call audio output
            <select
              value={call.outputDeviceId}
              onchange={(e) => store.setOutputDevice(e.currentTarget.value)}
            >
              <option value="">Default</option>
              {#each call.outputDevices as d (d.deviceId)}
                {#if d.deviceId && d.deviceId !== 'default'}
                  <option value={d.deviceId}>{d.label}</option>
                {/if}
              {/each}
            </select>
          </label>
        {/if}
        {#if uploadEstimate > 0}
          <span class="estimate">≈{uploadEstimate.toFixed(0)} Mb/s upload ({call.relay?.outgoing ? 'relay tree' : `${call.participants.length - 1} viewer${call.participants.length === 2 ? '' : 's'}`})</span>
        {/if}
        {#if call.relay?.local.busy}
          <p class="hint">
            Game detected ({call.relay.local.reasons.join(', ')}): not passing others' relayed streams on,
            so your upload and CPU stay with the game.
          </p>
        {/if}
        {#if s.systemAudio && Object.keys(call.remoteStreams).length > 0}
          <p class="hint">
            Sharing system audio while hearing others can echo their voices back.
            Route “Call audio output” to a separate device to avoid the loop.
          </p>
        {/if}
      </div>
    {/if}

    <div class="tiles" class:has-spotlight={spotlightId != null}>
      {#if call.broadcasting && call.manager.localScreen}
        {@const id = `local:${call.manager.localScreen.id}`}
        <figure class="tile local" class:spotlight={spotlightId === id} class:dimmed={spotlightId != null && spotlightId !== id}>
          <!-- svelte-ignore a11y_media_has_caption -->
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <video autoplay playsinline muted controls use:srcObject={call.manager.localScreen} onclick={() => toggleSpotlight(id)} ondblclick={(e) => toggleFullscreen(e.currentTarget)} title={(spotlightId === id ? 'Click to shrink' : 'Click to enlarge') + ' · Double-click for fullscreen'}></video>
          <figcaption>You (preview) · {relayOutLine() || statLine(call.participants.find((p) => p !== call.selfId) ?? '')}</figcaption>
        </figure>
      {/if}
      {#each Object.entries(call.remoteStreams) as [userId, streams] (userId)}
        {#each streams as stream (stream.id)}
          {#if hasVideo(stream)}
            <figure class="tile" class:spotlight={spotlightId === stream.id} class:dimmed={spotlightId != null && spotlightId !== stream.id}>
              <!-- svelte-ignore a11y_media_has_caption -->
              <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
              <!-- Muted: this peer's audio is played (and volume-controlled) by the mixer. -->
              <video autoplay playsinline use:srcObject={stream} onclick={() => toggleSpotlight(stream.id)} ondblclick={(e) => toggleFullscreen(e.currentTarget)} title={(spotlightId === stream.id ? 'Click to shrink' : 'Click to enlarge') + ' · Double-click for fullscreen'}></video>
              <figcaption>{name(userId)} · {statLine(userId)}</figcaption>
            </figure>
          {/if}
        {/each}
      {/each}
    </div>
  </section>
{/if}

<style>
  .panel {
    border-bottom: 1px solid var(--bg-3);
    background: var(--bg-1);
    padding: 10px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .controls { justify-content: space-between; }
  .participants { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; }
  .chip {
    background: var(--bg-3);
    color: var(--fg-0);
    border-radius: 999px;
    padding: 3px 10px;
    font-size: 12.5px;
    line-height: 1.5;
  }
  .chip.me { outline: 1px solid var(--accent); }
  button.chip { display: inline-flex; align-items: center; gap: 5px; }
  button.chip.adjusted { outline: 1px solid var(--fg-1); }
  .vol-badge { color: var(--fg-1); font-size: 11px; }

  .chip-wrap { position: relative; }
  .vol-pop {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 30;
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--bg-1);
    border: 1px solid var(--bg-3);
    border-radius: var(--radius);
    padding: 8px 10px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
  }
  .vol-pop input[type='range'] {
    width: 130px;
    padding: 0;
    border: none;
    background: transparent;
    accent-color: var(--accent);
  }
  .vol-num { font-size: 12px; color: var(--fg-1); min-width: 34px; text-align: right; }

  .buttons { display: flex; gap: 6px; }
  .buttons button.active { background: var(--accent); color: #0d1117; font-weight: 600; }

  .settings label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 11.5px;
    color: var(--fg-1);
  }
  .settings label.check { flex-direction: row; align-items: center; gap: 6px; font-size: 13px; }
  .estimate { color: var(--fg-1); font-size: 12px; margin-left: auto; }
  .settings .hint {
    flex-basis: 100%;
    margin: 0;
    color: var(--fg-1);
    font-size: 11.5px;
    line-height: 1.4;
  }

  .tiles { display: flex; gap: 10px; flex-wrap: wrap; align-items: flex-start; }
  .tile { margin: 0; max-width: 480px; flex: 1 1 320px; }
  .tile video {
    width: 100%;
    border-radius: var(--radius);
    background: #000;
    aspect-ratio: 16 / 9;
    cursor: zoom-in;
    display: block;
  }
  .tile.local video { opacity: 0.9; }

  /* Spotlight: clicked tile fills the row; the rest shrink to a thumbnail strip. */
  .tiles.has-spotlight { flex-wrap: nowrap; }
  .tile.spotlight {
    max-width: none;
    flex: 1 1 100%;
    order: -1;
  }
  .tile.spotlight video {
    cursor: zoom-out;
    max-height: 70vh;
    object-fit: contain;
  }
  .tile.dimmed { flex: 0 0 200px; max-width: 200px; }
  figcaption { font-size: 11.5px; color: var(--fg-1); margin-top: 2px; }
</style>

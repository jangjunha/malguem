<script lang="ts">
  import Icon from './Icon.svelte';
  import VolumePopover from './VolumePopover.svelte';
  import { canRestrictOwnAudio } from '../lib/call';
  import { formatAccelerator } from '../lib/keybinds';
  import { store } from '../lib/store.svelte';

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

  /** Local preview caption: what the capture really delivers + our send stats. */
  function localLine(): string {
    const cap = call?.manager.captureSettings();
    const capture = cap ? `capture ${cap.width}×${cap.height} @ ${cap.frameRate}fps` : '';
    const send = relayOutLine() || statLine(call?.participants.find((p) => p !== call?.selfId) ?? '');
    return [capture, send].filter(Boolean).join(' · ');
  }

  function isMuted(userId: string): boolean {
    return userId === call?.selfId ? store.micMuted : call?.peerStates[userId]?.muted === true;
  }
  function isDeafened(userId: string): boolean {
    return userId === call?.selfId ? store.deafened : call?.peerStates[userId]?.deafened === true;
  }

  const hasTiles = $derived(
    !!call &&
      ((call.broadcasting && !!call.manager.localScreen) ||
        Object.values(call.remoteStreams).some((list) => list.some(hasVideo))),
  );

  /** Sharing system audio without a way to keep our own playback out of it. */
  const loopbackRisk = $derived(
    !!call &&
      call.broadcasting &&
      s.systemAudio &&
      call.manager.ownAudioExcluded === false &&
      call.participants.length > 1,
  );

  const uploadEstimate = $derived(
    call && call.broadcasting
      ? call.relay?.outgoing
        ? (s.maxBitrateKbps * Math.max(1, Object.values(call.relay.outgoing.tree).filter((p) => p === call.selfId).length)) / 1000
        : (s.maxBitrateKbps * Math.max(call.participants.length - 1, 1)) / 1000
      : 0,
  );
</script>

{#if call}
  <section class="panel">
    {#if hasTiles}
    <div class="tiles" class:has-spotlight={spotlightId != null}>
      {#if call.broadcasting && call.manager.localScreen}
        {@const id = `local:${call.manager.localScreen.id}`}
        <figure class="tile local" class:spotlight={spotlightId === id} class:dimmed={spotlightId != null && spotlightId !== id}>
          <!-- svelte-ignore a11y_media_has_caption -->
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <video autoplay playsinline muted use:srcObject={call.manager.localScreen} onclick={() => toggleSpotlight(id)} ondblclick={(e) => toggleFullscreen(e.currentTarget)} title={(spotlightId === id ? 'Click to shrink' : 'Click to enlarge') + ' · Double-click for fullscreen'}></video>
          <figcaption><b>You</b> (preview) · {localLine()}</figcaption>
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
              <figcaption><b>{name(userId)}</b> · {statLine(userId)}</figcaption>
            </figure>
          {/if}
        {/each}
      {/each}
    </div>
    {/if}

    <div class="people">
      {#each call.participants as p (p)}
        {@const self = p === call.selfId}
        <div class="person-wrap">
          <button
            class="person"
            class:speaking={store.speaking[p]}
            class:self
            disabled={self}
            title={self ? 'You' : `${name(p)} — click to adjust volume`}
            onclick={() => (volumeFor = volumeFor === p ? null : p)}
            oncontextmenu={(e) => {
              if (self) return;
              e.preventDefault();
              volumeFor = p;
            }}
          >
            <span class="avatar">{name(p).slice(0, 1).toUpperCase()}</span>
            <span class="pname">{name(p)}{self ? ' (you)' : ''}</span>
            {#if isMuted(p)}<span class="state" title="Muted"><Icon name="mic-off" size={14} /></span>{/if}
            {#if isDeafened(p)}<span class="state" title="Deafened"><Icon name="headphones-off" size={14} /></span>{/if}
            {#if !self && volumePct(p) !== 100}<span class="vol-badge">{volumePct(p)}%</span>{/if}
          </button>
          {#if volumeFor === p && !self}
            <VolumePopover userId={p} name={name(p)} onclose={() => (volumeFor = null)} />
          {/if}
        </div>
      {/each}
    </div>

    {#if loopbackRisk}
      <p class="warn">
        {canRestrictOwnAudio()
          ? "Your share's system audio couldn't exclude this app's own sound,"
          : "This app version can't keep call audio out of your shared system audio,"}
        so others may hear themselves. Set a separate <b>Output device</b> in
        <button class="inline-link" onclick={() => (store.settingsOpen = 'voice')}>Settings</button>
        or turn off Game/system audio.
      </p>
    {/if}

    <div class="controlbar">
      <button
        class="round"
        class:off={store.micMuted}
        aria-pressed={store.micMuted}
        onclick={() => store.toggleMic()}
        title="{store.micMuted ? 'Unmute' : 'Mute'} ({formatAccelerator(store.keybinds.toggleMute)})"
      >
        <Icon name={store.micMuted ? 'mic-off' : 'mic'} size={20} />
      </button>
      <button
        class="round"
        class:off={store.deafened}
        aria-pressed={store.deafened}
        onclick={() => store.toggleDeafen()}
        title="{store.deafened ? 'Undeafen' : 'Deafen'} ({formatAccelerator(store.keybinds.toggleDeafen)})"
      >
        <Icon name={store.deafened ? 'headphones-off' : 'headphones'} size={20} />
      </button>
      <button
        class="pill"
        class:on={call.broadcasting}
        disabled={call.broadcastStarting}
        onclick={() => store.toggleBroadcast()}
        title={call.broadcasting ? 'Stop sharing' : 'Share your screen'}
      >
        <Icon name={call.broadcasting ? 'screen-off' : 'screen'} size={20} />
        {call.broadcastStarting ? 'Choosing…' : call.broadcasting ? 'Stop sharing' : 'Share screen'}
      </button>
      <button class="round" class:sel={showSettings} onclick={() => (showSettings = !showSettings)} title="Stream quality settings">
        <Icon name="sliders" size={20} />
      </button>
      <button class="round hang" onclick={() => store.leaveCall()} title="Leave call">
        <Icon name="hangup" size={20} />
      </button>
    </div>

    {#if showSettings}
      <div class="settings">
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
        {#if uploadEstimate > 0}
          <span class="estimate">≈{uploadEstimate.toFixed(0)} Mb/s upload ({call.relay?.outgoing ? 'relay tree' : `${call.participants.length - 1} viewer${call.participants.length === 2 ? '' : 's'}`})</span>
        {/if}
        {#if s.frameRate === 60}
          <p class="hint">
            60 fps needs bits: at 1080p use 8 Mb/s or more, or frames get dropped
            (the preview caption shows what the capture and encoder actually deliver).
          </p>
        {/if}
        {#if call.relay?.local.busy}
          <p class="hint">
            Game detected ({call.relay.local.reasons.join(', ')}): not passing others' relayed streams on,
            so your upload and CPU stay with the game.
          </p>
        {/if}
      </div>
    {/if}
  </section>
{/if}

<style>
  .panel {
    flex: none;
    border-bottom: 1px solid var(--bg-3);
    background: var(--bg-1);
    padding: 12px 16px 10px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .people { display: flex; gap: 8px; flex-wrap: wrap; justify-content: center; }
  .person-wrap { position: relative; }
  .person {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--bg-2);
    border-radius: 999px;
    padding: 4px 12px 4px 4px;
    font-size: 13px;
    box-shadow: 0 0 0 2px transparent;
    transition: box-shadow 80ms;
  }
  .person:disabled { opacity: 1; cursor: default; }
  .person.speaking { box-shadow: 0 0 0 2px var(--ok); }
  .avatar {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--bg-3);
    display: grid;
    place-items: center;
    font-weight: 700;
    font-size: 12px;
  }
  .person.self .avatar { background: var(--accent); color: #0d1117; }
  .pname { max-width: 160px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .state { color: var(--danger); display: inline-flex; }
  .vol-badge { color: var(--fg-1); font-size: 11px; }

  .controlbar { display: flex; justify-content: center; align-items: center; gap: 10px; }
  .controlbar button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    height: 44px;
    background: var(--bg-3);
    color: var(--fg-0);
  }
  .round { width: 44px; padding: 0; border-radius: 50%; }
  .pill { border-radius: 22px; padding: 0 18px; font-weight: 600; }
  .controlbar .off { background: var(--danger); color: #0d1117; }
  .controlbar .on { background: var(--ok); color: #0d1117; }
  .controlbar .sel { outline: 2px solid var(--accent); }
  .controlbar .hang { background: var(--danger); color: #0d1117; }

  .settings {
    display: flex;
    align-items: flex-end;
    gap: 10px;
    flex-wrap: wrap;
    background: var(--bg-2);
    border-radius: var(--radius);
    padding: 10px 12px;
  }
  .settings label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 11.5px;
    color: var(--fg-1);
  }
  .settings label.check { flex-direction: row; align-items: center; gap: 6px; font-size: 13px; align-self: center; }
  .estimate { color: var(--fg-1); font-size: 12px; margin-left: auto; align-self: center; }
  .hint, .warn {
    flex-basis: 100%;
    margin: 0;
    color: var(--fg-1);
    font-size: 11.5px;
    line-height: 1.4;
  }
  .warn {
    color: var(--fg-0);
    background: color-mix(in srgb, var(--danger) 18%, transparent);
    border-radius: var(--radius);
    padding: 6px 10px;
    font-size: 12px;
  }
  .inline-link { background: none; padding: 0; color: var(--accent); text-decoration: underline; font-size: inherit; }

  /* Videos get a bounded, scrollable area so the chat below always keeps room
     (and popovers in the rows below aren't clipped by a scroll container). */
  .tiles {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    align-items: flex-start;
    justify-content: center;
    max-height: 48vh;
    overflow-y: auto;
  }
  .tile { margin: 0; max-width: 560px; flex: 1 1 320px; }
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
    max-height: 46vh;
    object-fit: contain;
  }
  .tile.dimmed { flex: 0 0 200px; max-width: 200px; }
  figcaption { font-size: 11.5px; color: var(--fg-1); margin-top: 2px; }
  figcaption b { color: var(--fg-0); }
</style>

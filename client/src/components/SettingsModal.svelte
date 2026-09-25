<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import Modal from './Modal.svelte';
  import { ALWAYS_OPEN_DB, AUTO_MIN_DB } from '../lib/gate';
  import { defaultBinding, eventToAccelerator, formatAccelerator } from '../lib/keybinds';
  import { MAX_GAIN } from '../lib/mixer';
  import { isTauri } from '../lib/platform';
  import { store } from '../lib/store.svelte';

  const v = $derived(store.voice);
  const kb = $derived(store.keybinds);
  const tab = $derived(store.settingsOpen ?? 'voice');

  /** Meter scale: -80 dBFS … 0 dBFS → 0 … 100%. */
  const FLOOR_DB = ALWAYS_OPEN_DB;
  const pos = (db: number) => Math.max(0, Math.min(100, ((db - FLOOR_DB) / -FLOOR_DB) * 100));
  const level = $derived(store.micLevel);

  /** Which action is waiting for a key press. */
  let recording = $state<null | 'toggleMute' | 'toggleDeafen'>(null);

  onMount(() => {
    // Without a call, open the mic just to drive the meter.
    void store.startMicTest();
    void store.refreshDevices();
  });
  onDestroy(() => store.stopMicTest());

  function close() {
    store.settingsOpen = null;
  }

  function onKeyCapture(e: KeyboardEvent) {
    if (!recording) return;
    // Own the keystroke: no modal Escape, no hotkey firing while rebinding.
    e.preventDefault();
    e.stopImmediatePropagation();
    if (e.key === 'Escape') {
      recording = null;
      return;
    }
    const acc = eventToAccelerator(e);
    if (!acc) return; // still holding modifiers
    store.keybinds[recording] = acc;
    recording = null;
    store.saveKeybinds();
  }

  function setBinding(action: 'toggleMute' | 'toggleDeafen', acc: string) {
    store.keybinds[action] = acc;
    store.saveKeybinds();
  }

  const labels = { toggleMute: 'Toggle mute', toggleDeafen: 'Toggle deafen' } as const;
</script>

<svelte:window onkeydowncapture={onKeyCapture} />

<Modal title="Settings" onclose={close} width={520}>
  <div class="tabs" role="tablist">
    <button role="tab" aria-selected={tab === 'voice'} class:sel={tab === 'voice'} onclick={() => (store.settingsOpen = 'voice')}>Voice</button>
    <button role="tab" aria-selected={tab === 'keybinds'} class:sel={tab === 'keybinds'} onclick={() => (store.settingsOpen = 'keybinds')}>Keybinds</button>
  </div>

  {#if tab === 'voice'}
    <div class="grid2">
      <label>
        Input device
        <select bind:value={store.voice.inputDeviceId} onchange={() => store.applyVoice()}>
          <option value="">Default</option>
          {#each store.inputDevices as d (d.deviceId)}
            <option value={d.deviceId}>{d.label}</option>
          {/each}
        </select>
      </label>
      {#if store.outputDevices.length > 0}
        <label>
          Output device
          <select value={v.outputDeviceId} onchange={(e) => store.setOutputDevice(e.currentTarget.value)}>
            <option value="">Default</option>
            {#each store.outputDevices as d (d.deviceId)}
              <option value={d.deviceId}>{d.label}</option>
            {/each}
          </select>
        </label>
      {/if}
    </div>

    <label>
      <span class="row">Input volume <span class="num">{Math.round(v.inputGain * 100)}%</span></span>
      <input
        type="range"
        min="0"
        max={MAX_GAIN * 100}
        step="5"
        value={Math.round(v.inputGain * 100)}
        oninput={(e) => {
          store.voice.inputGain = e.currentTarget.valueAsNumber / 100;
          void store.applyVoice();
        }}
      />
    </label>

    <div class="field">
      <span class="row">
        Input sensitivity
        <label class="check inline">
          <input
            type="checkbox"
            bind:checked={store.voice.gate.auto}
            onchange={() => store.applyVoice()}
          />
          Automatic
        </label>
      </span>
      <!-- The meter: live level, with the threshold marked. Green while the mic is transmitting. -->
      <div class="meter" class:open={level?.open}>
        <div class="fill" style:width="{pos(level?.levelDb ?? -100)}%"></div>
        <div class="threshold" style:left="{pos(level?.thresholdDb ?? (v.gate.auto ? AUTO_MIN_DB : v.gate.thresholdDb))}%"></div>
      </div>
      {#if !v.gate.auto}
        <input
          type="range"
          min={FLOOR_DB}
          max="0"
          step="1"
          value={v.gate.thresholdDb}
          oninput={(e) => {
            store.voice.gate.thresholdDb = e.currentTarget.valueAsNumber;
            void store.applyVoice();
          }}
          aria-label="Sensitivity threshold"
        />
        <p class="hint">
          {v.gate.thresholdDb <= ALWAYS_OPEN_DB
            ? 'Always transmitting.'
            : `Your mic only transmits while the bar passes the marker (${v.gate.thresholdDb} dB).`}
          Drag fully left to always transmit.
        </p>
      {:else}
        <p class="hint">Picks up your voice above the background noise automatically.</p>
      {/if}
      {#if !level}
        <p class="hint">Opening microphone…</p>
      {/if}
    </div>

    <div class="checks">
      <label class="check">
        <input type="checkbox" bind:checked={store.voice.echoCancellation} onchange={() => store.applyVoice()} />
        Echo cancellation
      </label>
      <label class="check">
        <input type="checkbox" bind:checked={store.voice.noiseSuppression} onchange={() => store.applyVoice()} />
        Noise suppression
      </label>
      <label class="check" title="Turn off to control your volume manually with Input volume">
        <input type="checkbox" bind:checked={store.voice.autoGainControl} onchange={() => store.applyVoice()} />
        Automatic gain control
      </label>
    </div>
  {:else}
    <div class="binds">
      {#each ['toggleMute', 'toggleDeafen'] as const as action (action)}
        <div class="bind">
          <span class="bind-name">{labels[action]}</span>
          <button class="key" class:rec={recording === action} onclick={() => (recording = recording === action ? null : action)}>
            {recording === action ? 'Press a key combo… (Esc to cancel)' : formatAccelerator(kb[action])}
          </button>
          <button class="link" title="Remove shortcut" onclick={() => setBinding(action, '')}>Clear</button>
          <button class="link" title="Back to default" onclick={() => setBinding(action, defaultBinding(action))}>Reset</button>
        </div>
      {/each}
    </div>
    {#if isTauri()}
      <label class="check">
        <input type="checkbox" bind:checked={store.keybinds.global} onchange={() => store.saveKeybinds()} />
        Work while other apps (games) have focus, during calls
      </label>
      {#each store.hotkeyStatus.errors as err (err)}
        <p class="hint warn">Couldn't register {err}. It still works while this window is focused.</p>
      {/each}
    {:else}
      <p class="hint">Shortcuts work while this window is focused (OS-wide shortcuts need the desktop app).</p>
    {/if}
  {/if}
</Modal>

<style>
  .tabs { display: flex; gap: 4px; border-bottom: 1px solid var(--bg-3); margin: -4px 0 4px; }
  .tabs button {
    background: transparent;
    color: var(--fg-1);
    border-radius: 0;
    border-bottom: 2px solid transparent;
    padding: 6px 10px;
  }
  .tabs button.sel { color: var(--fg-0); border-bottom-color: var(--accent); }

  label, .field { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--fg-1); }
  .grid2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .grid2 select { width: 100%; min-width: 0; }
  .row { display: flex; justify-content: space-between; align-items: center; gap: 8px; }
  .num { font-variant-numeric: tabular-nums; }
  input[type='range'] { width: 100%; padding: 0; border: none; background: transparent; accent-color: var(--accent); }
  label.check { flex-direction: row; align-items: center; gap: 6px; font-size: 13px; color: var(--fg-0); }
  label.check.inline { font-size: 12px; color: var(--fg-1); }
  .checks { display: flex; flex-direction: column; gap: 6px; }

  .meter {
    position: relative;
    height: 10px;
    border-radius: 5px;
    background: var(--bg-0);
    overflow: hidden;
  }
  .meter .fill {
    height: 100%;
    background: #d29922;
    transition: width 60ms linear;
  }
  .meter.open .fill { background: var(--ok); }
  .meter .threshold {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin-left: -1px;
    background: var(--fg-0);
  }
  .hint { margin: 0; font-size: 11.5px; color: var(--fg-1); line-height: 1.4; }
  .hint.warn { color: var(--danger); }

  .binds { display: flex; flex-direction: column; gap: 8px; }
  .bind { display: flex; align-items: center; gap: 10px; }
  .bind-name { flex: 1; }
  .key {
    min-width: 150px;
    font-family: ui-monospace, monospace;
    font-size: 12.5px;
    background: var(--bg-0);
    border: 1px solid var(--bg-3);
  }
  .key.rec { border-color: var(--accent); color: var(--accent); }
  .link { background: transparent; color: var(--fg-1); font-size: 12px; padding: 2px; }
  .link:hover { color: var(--fg-0); }
</style>

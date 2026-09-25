import { describe, expect, it } from 'vitest';
import { visibleStreams } from './call';

type T = { kind: 'audio' | 'video'; muted?: boolean; ended?: boolean };

let nextId = 0;
function stream(...tracks: T[]) {
  const ts = tracks.map((t) => ({
    kind: t.kind,
    muted: t.muted ?? false,
    readyState: (t.ended ? 'ended' : 'live') as MediaStreamTrackState,
  }));
  return {
    id: `s${nextId++}`,
    getTracks: () => ts,
    getVideoTracks: () => ts.filter((t) => t.kind === 'video'),
  };
}

describe('visibleStreams', () => {
  it('always keeps an audio-only voice stream', () => {
    const s = stream({ kind: 'audio' });
    expect(visibleStreams([s])).toEqual([s]);
  });

  it('keeps a screen share while its video is live', () => {
    const s = stream({ kind: 'video' }, { kind: 'audio' });
    expect(visibleStreams([s])).toEqual([s]);
  });

  it('hides a screen share once its video track is muted (sharer stopped)', () => {
    const s = stream({ kind: 'video', muted: true }, { kind: 'audio' });
    expect(visibleStreams([s])).toEqual([]);
  });

  it('hides a screen share whose video track has ended', () => {
    const s = stream({ kind: 'video', ended: true });
    expect(visibleStreams([s])).toEqual([]);
  });

  it('drops a stream with no tracks left', () => {
    expect(visibleStreams([stream()])).toEqual([]);
  });

  it('shows only the fresh re-share, not the stale frozen one', () => {
    const stale = stream({ kind: 'video', muted: true }, { kind: 'audio' });
    const fresh = stream({ kind: 'video' }, { kind: 'audio' });
    expect(visibleStreams([stale, fresh])).toEqual([fresh]);
  });
});

describe('CallManager.startBroadcast', () => {
  it('captures once when Share is clicked again while the picker is open, and Stop ends it', async () => {
    const { CallManager } = await import('./call');
    const stopped: string[] = [];
    const fakeTrack = (id: string) => ({
      id,
      kind: 'video',
      contentHint: '',
      onended: null as null | (() => void),
      stop: () => stopped.push(id),
      getSettings: () => ({ width: 1920, height: 1080, frameRate: 60 }),
      applyConstraints: async () => {},
    });
    let captures = 0;
    let release: (() => void) | null = null;
    const getDisplayMedia = () => {
      captures++;
      const track = fakeTrack(`v${captures}`);
      const s = { getVideoTracks: () => [track], getAudioTracks: () => [], getTracks: () => [track] };
      return new Promise((resolve) => (release = () => resolve(s)));
    };
    const g = globalThis as unknown as { navigator?: unknown };
    const prevNav = g.navigator;
    Object.defineProperty(globalThis, 'navigator', {
      value: { mediaDevices: { getDisplayMedia } },
      configurable: true,
    });
    try {
      const socket = { onEvent: () => () => {}, onOpen: () => () => {}, send: () => {} };
      const m = new CallManager(
        socket as never, {} as never, 'me', 'space', 'chan', () => undefined, [],
        {
          onPeersChanged() {}, onRemoteStreams() {}, onStats() {}, onBroadcastChanged() {},
          onPeerJoined() {}, onPeerLeft() {}, onEnded() {},
        },
      );
      (m as unknown as { joined: boolean }).joined = true;
      const a = m.startBroadcast();
      const b = m.startBroadcast(); // double click
      expect(m.isStartingBroadcast).toBe(true);
      release!();
      await Promise.all([a, b]);
      expect(captures).toBe(1);
      expect(m.isBroadcasting).toBe(true);
      m.stopBroadcast();
      expect(m.isBroadcasting).toBe(false);
      expect(stopped).toEqual(['v1']);

      // Stop pressed while the picker is still open: the late capture is discarded.
      const c = m.startBroadcast();
      m.stopBroadcast();
      release!();
      await c;
      expect(m.isBroadcasting).toBe(false);
      expect(stopped).toEqual(['v1', 'v2']);
    } finally {
      Object.defineProperty(globalThis, 'navigator', { value: prevNav, configurable: true });
    }
  });
});

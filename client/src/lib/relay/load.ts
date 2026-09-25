/**
 * Local load: is this machine busy (a game running, CPU under pressure)?
 *
 * The relay engine asks viewers to spend CPU (forwarding, decoding) and,
 * above all, upload on others' behalf. On a gaming PC that upload fills the
 * home router's buffer and raises the game's ping, so while the machine is
 * busy it stops relaying and keeps its own share of the stream light.
 *
 * Signals, any of which marks the machine busy:
 * - a fullscreen app (game) in the foreground, from the OS via the Tauri
 *   `fullscreen_app_active` command (ignored while *our* window is the
 *   fullscreen one, e.g. watching a stream fullscreen);
 * - the Compute Pressure API reporting 'serious' or 'critical' CPU pressure;
 * - the page's own event loop lagging (timers firing late), as a fallback
 *   where Compute Pressure isn't available.
 *
 * Busy starts immediately and ends only after CALM_MS without any signal, so
 * the tree doesn't reshuffle every time a game hitches or a menu opens.
 */

export type LoadReason = 'fullscreen-app' | 'cpu-pressure' | 'event-loop-lag';

export interface LocalLoad {
  busy: boolean;
  reasons: LoadReason[];
  /** CPU pressure is critical: shed decode work too. */
  critical: boolean;
  /** Our window isn't visible: nobody is looking at the stream. */
  hidden: boolean;
}

export interface LoadMonitorOptions {
  /** Asks the OS whether a fullscreen app is in front (Tauri); omit in browsers. */
  fullscreenApp?: () => Promise<boolean>;
  now?: () => number;
}

const CALM_MS = 15_000;
const LAG_SAMPLE_MS = 250;
const LAG_WINDOW = 20; // 5 s of samples
const LAG_BUSY_MS = 60; // p90 lateness that counts as a struggling machine
const FULLSCREEN_POLL_MS = 3000;

interface PressureRecordLike {
  source: string;
  state: 'nominal' | 'fair' | 'serious' | 'critical';
}

export class LoadMonitor {
  private lastSignalAt = new Map<LoadReason, number>();
  private active = new Set<LoadReason>();
  private critical = false;
  private lags: number[] = [];
  private timers: ReturnType<typeof setInterval>[] = [];
  private observer: { disconnect(): void } | null = null;
  private last: LocalLoad = { busy: false, reasons: [], critical: false, hidden: false };
  private now: () => number;

  constructor(
    private onChange: (load: LocalLoad) => void,
    private opts: LoadMonitorOptions = {},
  ) {
    this.now = opts.now ?? (() => performance.now());
  }

  get current(): LocalLoad {
    return this.last;
  }

  start(): void {
    // Compute Pressure (Chromium ≥ 125, so WebView2); absent elsewhere.
    const PO = (globalThis as unknown as { PressureObserver?: new (cb: (r: PressureRecordLike[]) => void) => { observe(s: string, o?: object): Promise<void>; disconnect(): void } }).PressureObserver;
    if (PO) {
      try {
        const obs = new PO((records) => {
          const r = records.at(-1);
          if (!r) return;
          const pressured = r.state === 'serious' || r.state === 'critical';
          this.critical = r.state === 'critical';
          this.signal('cpu-pressure', pressured);
        });
        obs.observe('cpu', { sampleInterval: 2000 }).catch(() => {});
        this.observer = obs;
      } catch {
        /* not permitted here */
      }
    }
    // Event-loop lag: how late a fixed-rate timer fires.
    let expected = this.now() + LAG_SAMPLE_MS;
    this.timers.push(
      setInterval(() => {
        const t = this.now();
        this.lags.push(Math.max(0, t - expected));
        expected = t + LAG_SAMPLE_MS;
        if (this.lags.length > LAG_WINDOW) this.lags.shift();
        if (this.lags.length === LAG_WINDOW) {
          const sorted = [...this.lags].sort((a, b) => a - b);
          this.signal('event-loop-lag', sorted[Math.floor(LAG_WINDOW * 0.9)]! > LAG_BUSY_MS);
        }
        this.evaluate();
      }, LAG_SAMPLE_MS),
    );
    if (this.opts.fullscreenApp) {
      const poll = async () => {
        // Our own fullscreen stream tile isn't a game.
        const ours = typeof document !== 'undefined' && document.fullscreenElement !== null;
        let fs = false;
        try {
          fs = !ours && (await this.opts.fullscreenApp!());
        } catch {
          /* command unavailable */
        }
        this.signal('fullscreen-app', fs);
      };
      void poll();
      this.timers.push(setInterval(() => void poll(), FULLSCREEN_POLL_MS));
    }
    if (typeof document !== 'undefined') document.addEventListener('visibilitychange', this.onVisibility);
  }

  stop(): void {
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
    this.observer?.disconnect();
    this.observer = null;
    if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', this.onVisibility);
  }

  /** Feed a signal directly (tests, simulator, or a manual "I'm gaming" toggle). */
  signal(reason: LoadReason, on: boolean): void {
    // Remember when a reason was last seen active: while on, and at the
    // moment it turns off (the calm period counts from then).
    if (on || this.active.has(reason)) this.lastSignalAt.set(reason, this.now());
    if (on) this.active.add(reason);
    else this.active.delete(reason);
    this.evaluate();
  }

  private onVisibility = () => this.evaluate();

  private evaluate(): void {
    const t = this.now();
    // A reason stays in effect until it has been quiet for CALM_MS.
    const reasons = [...this.lastSignalAt.entries()]
      .filter(([r, at]) => this.active.has(r) || t - at < CALM_MS)
      .map(([r]) => r)
      .sort();
    const hidden = typeof document !== 'undefined' && document.visibilityState === 'hidden';
    const next: LocalLoad = { busy: reasons.length > 0, reasons, critical: this.critical && reasons.includes('cpu-pressure'), hidden };
    const prev = this.last;
    if (
      next.busy !== prev.busy ||
      next.critical !== prev.critical ||
      next.hidden !== prev.hidden ||
      next.reasons.join() !== prev.reasons.join()
    ) {
      this.last = next;
      this.onChange(next);
    }
  }
}

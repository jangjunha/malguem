<script lang="ts">
  import { tick } from 'svelte';
  import CallPanel from './CallPanel.svelte';
  import MessageContent from './MessageContent.svelte';
  import Sticker from './Sticker.svelte';
  import StickerPicker from './StickerPicker.svelte';
  import type { Space } from '../lib/api';
  import { replaceEmojiShortcodes, searchEmoji, type EmojiHit } from '../lib/emoji';
  import { store } from '../lib/store.svelte';

  let { serverId, channelId, space }: { serverId: string; channelId: string; space: Space } = $props();

  let draft = $state('');
  let scroller = $state<HTMLElement | null>(null);
  let inputEl = $state<HTMLInputElement | null>(null);
  let showStickers = $state(false);

  // Emoji shortcode autocomplete state (`:joy` → 😂).
  let emojiQuery = $state<string | null>(null);
  let emojiHits = $state<EmojiHit[]>([]);
  let emojiSel = $state(0);

  const channel = $derived(space.channels.find((c) => c.id === channelId));
  const msgs = $derived(store.messagesOf(serverId, channelId));
  const inThisCall = $derived(store.call?.serverId === serverId && store.call?.channelId === channelId);
  const locked = $derived(store.lockedOf(serverId, space.id));
  const isOwner = $derived(space.owner_id === store.userIdOf(serverId));
  const presence = $derived(store.presenceOf(serverId, channelId));

  // ---- scrolling ----
  // Follow new messages only while the reader is at the bottom; if they've
  // scrolled up to read history, leave them there and offer a jump button.
  // Content that grows after render (images, link cards, stickers) and the
  // call panel resizing the viewport both keep a pinned view pinned.
  let content = $state<HTMLElement | null>(null);
  let pinned = $state(true);
  let unseen = $state(0);
  const PIN_SLACK_PX = 48;

  function scrollToBottom() {
    if (!scroller) return;
    scroller.scrollTop = scroller.scrollHeight;
    pinned = true;
    unseen = 0;
  }

  function onScroll() {
    if (!scroller) return;
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= PIN_SLACK_PX;
    if (pinned) unseen = 0;
  }

  // New channel: start at its latest message.
  let shownChannel = '';
  let shownCount = 0;
  $effect(() => {
    const count = msgs.length;
    const key = `${serverId}|${channelId}`;
    if (!scroller) return;
    if (key !== shownChannel) {
      shownChannel = key;
      shownCount = count;
      scrollToBottom();
      return;
    }
    const added = count - shownCount;
    shownCount = count;
    if (added <= 0) return;
    const mine = msgs[count - 1]?.senderId === store.userIdOf(serverId);
    if (pinned || mine) scrollToBottom();
    else unseen += added;
  });

  $effect(() => {
    if (!scroller || !content) return;
    const ro = new ResizeObserver(() => {
      if (pinned && scroller) scroller.scrollTop = scroller.scrollHeight;
    });
    ro.observe(scroller);
    ro.observe(content);
    return () => ro.disconnect();
  });

  /** Recompute the emoji menu from the `:token` immediately before the caret. */
  function refreshEmoji() {
    const el = inputEl;
    if (!el) {
      emojiQuery = null;
      return;
    }
    const caret = el.selectionStart ?? draft.length;
    const before = draft.slice(0, caret);
    // A colon that starts a word (line start or after whitespace), then a run of
    // shortcode characters. This avoids matching `https://` or times like 10:30.
    const m = before.match(/(?:^|\s):([a-z0-9_+-]+)$/i);
    const q = m?.[1];
    if (q) {
      emojiQuery = q;
      emojiHits = searchEmoji(q);
      emojiSel = 0;
    } else {
      emojiQuery = null;
      emojiHits = [];
    }
  }

  async function acceptEmoji(hit: EmojiHit) {
    const el = inputEl;
    if (!el || emojiQuery === null) return;
    const caret = el.selectionStart ?? draft.length;
    const start = caret - emojiQuery.length - 1; // include the leading ':'
    draft = draft.slice(0, start) + hit.char + draft.slice(caret);
    emojiQuery = null;
    emojiHits = [];
    await tick();
    const pos = start + hit.char.length;
    el.focus();
    el.setSelectionRange(pos, pos);
  }

  function onComposerKeydown(e: KeyboardEvent) {
    if (emojiQuery === null || emojiHits.length === 0) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      emojiSel = (emojiSel + 1) % emojiHits.length;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      emojiSel = (emojiSel - 1 + emojiHits.length) % emojiHits.length;
    } else if (e.key === 'Enter' || e.key === 'Tab') {
      // Take the highlighted emoji instead of submitting / leaving the field.
      e.preventDefault();
      const hit = emojiHits[emojiSel];
      if (hit) void acceptEmoji(hit);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      emojiQuery = null;
      emojiHits = [];
    }
  }

  async function send(e: SubmitEvent) {
    e.preventDefault();
    // Expand any fully-typed `:shortcode:` left in the draft on send.
    const text = replaceEmojiShortcodes(draft).trim();
    if (!text) return;
    draft = '';
    emojiQuery = null;
    emojiHits = [];
    try {
      await store.sendMessage(serverId, channelId, text);
    } catch (err) {
      store.error = err instanceof Error ? err.message : String(err);
    }
  }

  async function pickSticker(stickerId: string) {
    showStickers = false;
    try {
      await store.sendSticker(serverId, channelId, stickerId);
    } catch (err) {
      store.error = err instanceof Error ? err.message : String(err);
    }
  }

  function time(ms: number): string {
    return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
</script>

<div class="view">
  <header>
    <span class="title"># {channel?.name}</span>
    {#if !inThisCall}
      <div class="join">
        {#if presence}
          <span class="in-call" title={presence.participants.map((p) => store.memberName(serverId, space.id, p)).join(', ')}>
            {#each presence.participants.slice(0, 4) as p (p)}
              <span class="mini">{store.memberName(serverId, space.id, p).slice(0, 1).toUpperCase()}</span>
            {/each}
            {presence.participants.length} in call{presence.streaming.length ? ' · LIVE' : ''}
          </span>
        {/if}
        <button
          class="primary"
          onclick={() => store.joinCall(serverId, channelId)}
          title={store.call ? 'Leaves your current call' : ''}
        >
          {store.call ? 'Switch to this call' : 'Join call'}
        </button>
      </div>
    {/if}
  </header>

  {#if inThisCall}
    <CallPanel />
  {/if}

  <div class="messages" bind:this={scroller} onscroll={onScroll}>
    <div class="content" bind:this={content}>
    {#if locked}
      <div class="locked">
        Waiting for another member to share this space's encryption key…
      </div>
    {/if}
    {#each msgs as m (m.id)}
      <div class="msg" class:bad={!m.ok}>
        <span class="meta"><b>{m.senderName}</b> {time(m.createdAt)}</span>
        <div class="body">
          {#if m.kind === 'sticker' && m.stickerId}
            <Sticker {serverId} spaceId={space.id} stickerId={m.stickerId} />
          {:else}
            <MessageContent text={m.body} />
          {/if}
        </div>
      </div>
    {/each}
    </div>
  </div>

  <div class="composer">
    {#if !pinned && unseen > 0}
      <button class="jump" onclick={scrollToBottom}>
        {unseen} new message{unseen === 1 ? '' : 's'} — jump to latest ↓
      </button>
    {/if}
    {#if showStickers}
      <StickerPicker
        {serverId}
        spaceId={space.id}
        {isOwner}
        onpick={pickSticker}
        onclose={() => (showStickers = false)}
      />
    {/if}
    {#if emojiQuery !== null && emojiHits.length > 0}
      <ul class="emoji-menu">
        {#each emojiHits as hit, i (hit.code)}
          <li>
            <button type="button" class:sel={i === emojiSel} onclick={() => acceptEmoji(hit)}>
              <span class="ch">{hit.char}</span>
              <span class="code">:{hit.code}:</span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <form onsubmit={send}>
      <button
        type="button"
        class="sticker-btn"
        title="Stickers"
        disabled={locked}
        onclick={() => (showStickers = !showStickers)}>🙂</button
      >
      <input
        bind:this={inputEl}
        bind:value={draft}
        oninput={refreshEmoji}
        onkeyup={refreshEmoji}
        onclick={refreshEmoji}
        onkeydown={onComposerKeydown}
        placeholder={locked ? 'Locked — no space key yet' : `Message #${channel?.name}`}
        disabled={locked}
        maxlength="4000"
      />
    </form>
  </div>
</div>

<style>
  .view { display: flex; flex-direction: column; height: 100%; min-height: 0; }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 16px;
    border-bottom: 1px solid var(--bg-3);
  }
  .title { font-weight: 600; }
  .join { display: flex; align-items: center; gap: 10px; }
  .in-call { display: flex; align-items: center; gap: 2px; color: var(--ok); font-size: 12.5px; }
  .in-call .mini {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--bg-3);
    color: var(--fg-0);
    display: grid;
    place-items: center;
    font-size: 10px;
    font-weight: 700;
    margin-right: -4px;
    box-shadow: 0 0 0 2px var(--bg-2);
  }
  .in-call .mini:last-of-type { margin-right: 6px; }

  .messages { flex: 1 1 0; min-height: 120px; overflow-y: auto; overflow-anchor: none; padding: 12px 16px; }
  .content { display: flex; flex-direction: column; gap: 8px; }
  .jump {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 50%;
    transform: translateX(-50%);
    z-index: 15;
    background: var(--accent);
    color: #0d1117;
    font-weight: 600;
    font-size: 12.5px;
    border-radius: 999px;
    padding: 5px 14px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.4);
  }
  .msg { display: flex; flex-direction: column; }
  .msg .meta { color: var(--fg-1); font-size: 12px; }
  .msg .meta b { color: var(--fg-0); }
  /* Text wrapping lives on MessageContent's own span; keep this a plain block
     so inter-element whitespace before OG cards/stickers isn't rendered. */
  .msg .body { white-space: normal; min-width: 0; }
  .msg.bad .body { color: var(--fg-1); font-style: italic; }
  .locked {
    background: var(--bg-3);
    border-radius: var(--radius);
    padding: 8px 12px;
    color: var(--fg-1);
    font-size: 13px;
  }

  .composer { padding: 12px 16px; position: relative; }
  .composer form { display: flex; gap: 8px; align-items: center; }
  .composer input { flex: 1; min-width: 0; }
  .sticker-btn {
    flex: none;
    background: var(--bg-3);
    font-size: 16px;
    line-height: 1;
    padding: 7px 9px;
  }

  .emoji-menu {
    position: absolute;
    bottom: calc(100% - 6px);
    left: 16px;
    right: 16px;
    margin: 0;
    padding: 4px;
    list-style: none;
    max-height: 220px;
    overflow-y: auto;
    background: var(--bg-1);
    border: 1px solid var(--bg-3);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    z-index: 20;
  }
  .emoji-menu li { display: block; }
  .emoji-menu button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    background: transparent;
    padding: 5px 8px;
    border-radius: 6px;
    color: var(--fg-0);
  }
  .emoji-menu button.sel,
  .emoji-menu button:hover { background: var(--bg-3); }
  .emoji-menu .ch { font-size: 16px; }
  .emoji-menu .code { color: var(--fg-1); font-size: 12.5px; }
</style>

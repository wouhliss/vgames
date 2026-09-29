// The overlay's toasts and panel. Server-provided text is rendered as plain text only.
import { useEffect, useRef, useState } from "react";
import { commands, events, type OverlayAction, type OverlayView } from "../bindings";

const EMPTY: OverlayView = {
  visible_panel: false,
  toasts: [],
  friends_online: [],
  invites: [],
  recent_messages: [],
};

const STATUS: Record<string, string> = {
  online: "Online",
  away: "Away",
  in_game: "In game",
  offline: "Offline",
};

function act(action: OverlayAction): void {
  void commands.overlayAction(action).catch(() => undefined);
}

/** Keeps the view model in sync with the Rust core. */
export function useOverlayView(): OverlayView {
  const [view, setView] = useState<OverlayView>(EMPTY);
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    void events.overlayView
      .listen((e) => {
        if (alive) setView(e.payload);
      })
      .then((u) => {
        if (alive) unlisten = u;
        else u();
      })
      .catch(() => undefined);
    void commands
      .overlayView()
      .then((v) => {
        if (alive) setView(v);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);
  return view;
}

function Reply({ conversationId, to }: { conversationId: string; to: string }) {
  const [text, setText] = useState("");
  return (
    <form
      className="ov-reply"
      onSubmit={(e) => {
        e.preventDefault();
        const t = text.trim();
        if (!t) return;
        act({ kind: "quick_reply", conversation_id: conversationId, text: t });
        setText("");
      }}
    >
      <input
        aria-label={`Reply to ${to}`}
        value={text}
        maxLength={500}
        onChange={(e) => setText(e.target.value)}
        placeholder="Reply…"
      />
      <button type="submit" disabled={!text.trim()}>
        Send
      </button>
    </form>
  );
}

function Panel({ view }: { view: OverlayView }) {
  const close = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    close.current?.focus();
  }, []);
  const lastByConversation = new Map<string, OverlayView["recent_messages"][number]>();
  for (const m of view.recent_messages) lastByConversation.set(m.conversation_id, m);
  return (
    <section
      className="ov-panel"
      aria-label="vgames overlay"
      onKeyDown={(e) => {
        if (e.key === "Escape") act({ kind: "close_panel" });
      }}
    >
      <header className="ov-head">
        <h1>vgames</h1>
        <button type="button" onClick={() => act({ kind: "open_launcher" })}>
          Open vgames
        </button>
        <button
          ref={close}
          type="button"
          aria-label="Close"
          onClick={() => act({ kind: "close_panel" })}
        >
          ×
        </button>
      </header>

      <h2>Invites</h2>
      {view.invites.length === 0 ? (
        <p className="ov-empty">No invites right now.</p>
      ) : (
        <ul className="ov-list">
          {view.invites.map((i) => (
            <li key={i.invite_id}>
              <span>
                <strong>{i.from}</strong> invites you to play <strong>{i.package_title}</strong>
              </span>
              {i.state === "pending" ? (
                <span className="ov-actions">
                  <button
                    type="button"
                    onClick={() => act({ kind: "accept_invite", invite_id: i.invite_id })}
                  >
                    Accept
                  </button>
                  <button
                    type="button"
                    onClick={() => act({ kind: "decline_invite", invite_id: i.invite_id })}
                  >
                    Decline
                  </button>
                </span>
              ) : (
                <span className="ov-muted">{i.state === "ready" ? "Ready" : "Getting ready…"}</span>
              )}
            </li>
          ))}
        </ul>
      )}

      <h2>Messages</h2>
      {lastByConversation.size === 0 ? (
        <p className="ov-empty">No new messages.</p>
      ) : (
        <ul className="ov-list">
          {[...lastByConversation.values()].map((m) => (
            <li key={m.conversation_id} className="ov-message">
              <span>
                <strong>{m.from}</strong>: {m.text}
              </span>
              <Reply conversationId={m.conversation_id} to={m.from} />
            </li>
          ))}
        </ul>
      )}

      <h2>Friends online</h2>
      {view.friends_online.length === 0 ? (
        <p className="ov-empty">No friends online.</p>
      ) : (
        <ul className="ov-list">
          {view.friends_online.map((f) => (
            <li key={f.user_id}>
              <strong>{f.name}</strong>
              <span className="ov-muted">
                {f.playing ? `Playing ${f.playing}` : STATUS[f.status]}
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export function OverlayApp() {
  const view = useOverlayView();
  return (
    <div className="ov-root">
      <ol className="ov-toasts" aria-live="polite">
        {view.toasts.map((t) => (
          <li key={t.id} className={`ov-toast ov-toast-${t.kind}`}>
            <strong>{t.title}</strong>
            <span>{t.body}</span>
          </li>
        ))}
      </ol>
      {view.visible_panel ? <Panel view={view} /> : null}
    </div>
  );
}

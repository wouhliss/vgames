// Add a friend (05-social-notes §1): create your own code (single use, 15 minutes, with a countdown
// and Copy), or type a friend's code. Input is normalized as Crockford base32 and sent once 8 valid
// characters are there; Rust validates again.
import { useEffect, useId, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { copyText } from "../../components/Feedback";
import { Notice } from "../../components/Notice";
import { TextField } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { t } from "../../i18n";
import { commands, type FriendCode } from "../../ipc";
import styles from "./Friends.module.css";
import {
  CODE_LENGTH,
  clock,
  displayName,
  groupCode,
  isCompleteCode,
  normalizeFriendCode,
  secondsUntil,
  socialErrorText,
} from "./model";

/** Re-renders every second while `active`, returning the current time. */
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return now;
}

function YourCode() {
  const [code, setCode] = useState<FriendCode | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const now = useNow(code !== null);
  const left = code ? secondsUntil(code.expires_at, now) : 0;
  const expired = code !== null && left === 0;

  const create = async () => {
    setBusy(true);
    setError(null);
    setCopied(false);
    try {
      const result = await commands.friendCodeCreate();
      if (result.status === "ok") setCode(result.data);
      else setError(socialErrorText(result.error));
    } catch {
      setError(t("friends.code.createFailed"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section aria-labelledby="your-code" className={styles.codeSection}>
      <h3 id="your-code">{t("friends.code.yours")}</h3>
      <p className={styles.muted}>{t("friends.code.yoursText")}</p>
      {error ? (
        <Notice tone="danger" role="alert">
          {error}
        </Notice>
      ) : null}
      {code && !expired ? (
        <div className={styles.codeBox}>
          <p className={styles.code} data-selectable="">
            {groupCode(code.code)}
          </p>
          {/* Announced once a minute at most: the visible countdown ticks every second. */}
          <p className={styles.muted} aria-live="off">
            {t("friends.code.expiresIn", { time: clock(left) })}
          </p>
          <Button icon="copy" onClick={() => void copyText(code.code).then((ok) => setCopied(ok))}>
            {copied ? t("friends.code.copied") : t("friends.code.copy")}
          </Button>
        </div>
      ) : (
        <div className={styles.codeBox}>
          {expired ? <p role="status">{t("friends.code.expired")}</p> : null}
          <Button variant="primary" loading={busy} onClick={() => void create()}>
            {expired ? t("friends.code.createAgain") : t("friends.code.create")}
          </Button>
        </div>
      )}
    </section>
  );
}

function TheirCode({ onSent }: { onSent: () => void }) {
  const { toast } = useToast();
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const sentFor = useRef<string | null>(null);
  const fieldId = useId();

  const send = async (code: string) => {
    if (!isCompleteCode(code) || busy) return;
    sentFor.current = code;
    setBusy(true);
    setError(null);
    try {
      const result = await commands.friendRequestSend({ kind: "code", code });
      if (result.status === "error") {
        setError(socialErrorText(result.error));
        return;
      }
      const name = displayName(result.data.user);
      toast({
        tone: "success",
        title:
          result.data.state === "accepted"
            ? t("friends.code.nowFriends", { name })
            : t("friends.code.sent", { name }),
      });
      onSent();
    } catch {
      setError(socialErrorText(null));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section aria-labelledby="their-code" className={styles.codeSection}>
      <h3 id="their-code">{t("friends.code.theirs")}</h3>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void send(value);
        }}
        className={styles.codeForm}
      >
        <TextField
          id={fieldId}
          label={t("friends.code.field")}
          description={t("friends.code.theirsText")}
          error={error}
          value={value}
          size="lg"
          autoComplete="off"
          autoCapitalize="characters"
          spellCheck={false}
          data-autofocus=""
          onChange={(e) => {
            const next = normalizeFriendCode(e.target.value);
            setValue(next);
            setError(null);
            // Submits by itself once complete (not again for the same code after an error).
            if (next.length === CODE_LENGTH && sentFor.current !== next) void send(next);
          }}
        />
        <Button type="submit" loading={busy} aria-disabled={!isCompleteCode(value) || undefined}>
          {t("friends.code.send")}
        </Button>
      </form>
    </section>
  );
}

export function AddFriendDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  return (
    <Dialog open={open} onClose={onClose} title={t("friends.code.title")} size="md">
      <div className={styles.stack}>
        <TheirCode onSent={onClose} />
        <YourCode />
      </div>
    </Dialog>
  );
}

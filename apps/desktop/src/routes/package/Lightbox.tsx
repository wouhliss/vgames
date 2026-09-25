// Full-size screenshots. Left/Right (arrow keys or the D-pad) and LB/RB step through them;
// Escape or B closes the viewer and returns focus to the thumbnail that opened it.
import { type KeyboardEvent, useEffect, useRef } from "react";
import { IconButton } from "../../components/Button";
import { Dialog } from "../../components/Dialog";
import { Glyph } from "../../components/Glyph";
import { t } from "../../i18n";
import type { Screenshot } from "../../ipc";
import { useOptionalNav } from "../../nav/NavProvider";
import styles from "./Package.module.css";

export function Lightbox({
  title,
  screenshots,
  index,
  onIndex,
  onClose,
}: {
  title: string;
  screenshots: readonly Screenshot[];
  index: number;
  onIndex: (index: number) => void;
  onClose: () => void;
}) {
  const total = screenshots.length;
  const shot = screenshots[index];
  const stepRef = useRef((delta: -1 | 1) => onIndex((index + delta + total) % total));
  stepRef.current = (delta) => onIndex((index + delta + total) % total);
  const frameRef = useRef<HTMLDivElement>(null);
  const registerTabs = useOptionalNav()?.registerTabs;

  // LB/RB (and Ctrl+PageUp/PageDown) move between screenshots while the viewer is open.
  useEffect(() => {
    const el = frameRef.current;
    if (!registerTabs || !el) return;
    return registerTabs(el, (dir) => stepRef.current(dir));
  }, [registerTabs]);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      stepRef.current(e.key === "ArrowLeft" ? -1 : 1);
    }
  };

  if (!shot) return null;
  return (
    <Dialog
      open
      size="xl"
      title={t("package.lightbox.title", { title })}
      description={t("package.lightbox.position", { n: index + 1, total })}
      onClose={onClose}
    >
      {/* biome-ignore lint/a11y/noStaticElementInteractions: routes arrow keys and the D-pad to the viewer's buttons. */}
      <div ref={frameRef} className={styles.lightbox} onKeyDown={onKeyDown}>
        <IconButton
          icon="chevronLeft"
          label={t("package.lightbox.previous")}
          size="lg"
          variant="secondary"
          onClick={() => stepRef.current(-1)}
          aria-disabled={total < 2 || undefined}
        />
        <figure className={styles.lightboxFigure}>
          <img
            src={shot.url}
            alt={t("package.screenshotLabel", { n: index + 1, total })}
            width={shot.width}
            height={shot.height}
            className={styles.lightboxImage}
          />
          <figcaption className={styles.lightboxCaption}>
            <Glyph action="tab_prev" /> {t("package.lightbox.position", { n: index + 1, total })}{" "}
            <Glyph action="tab_next" />
          </figcaption>
        </figure>
        <IconButton
          icon="chevronRight"
          label={t("package.lightbox.next")}
          size="lg"
          variant="secondary"
          onClick={() => stepRef.current(1)}
          aria-disabled={total < 2 || undefined}
          data-autofocus=""
        />
      </div>
    </Dialog>
  );
}

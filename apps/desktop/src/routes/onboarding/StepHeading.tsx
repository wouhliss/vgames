import { useEffect, useRef } from "react";
import styles from "./Onboarding.module.css";

/** Step title that receives focus when the step appears (unless a field asked for autofocus). */
export function StepHeading({ title, text }: { title: string; text?: string }) {
  const ref = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    const heading = ref.current;
    if (!heading) return;
    const card = heading.closest("form, section");
    const auto = card?.querySelector<HTMLElement>("[data-autofocus]");
    (auto ?? heading).focus({ preventScroll: true });
  }, []);
  return (
    <div className={styles.heading}>
      <h1 id="step-title" ref={ref} tabIndex={-1}>
        {title}
      </h1>
      {text ? <p className={styles.muted}>{text}</p> : null}
    </div>
  );
}

import type { ReactNode } from "react";
import styles from "../app/Shell.module.css";
import { EmptyState } from "../components/Feedback";
import { t } from "../i18n";

/** Standard page frame: a focusable h1 (so route changes can move focus to it) and content. */
export function Page({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className={styles.page}>
      <h1 tabIndex={-1} data-page-title="">
        {title}
      </h1>
      {children}
    </div>
  );
}

export function ComingSoon({ title }: { title: string }) {
  return (
    <Page title={title}>
      <EmptyState title={title} description={t("shell.placeholder")} />
    </Page>
  );
}

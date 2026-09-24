// Development-only gallery (/dev/gallery): every component in every state, reachable with the mouse,
// the keyboard and a controller. Not included in production builds.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { type ReactNode, useState } from "react";
import { useAppearanceQuery } from "../../app/appearance";
import { Button, IconButton } from "../../components/Button";
import { Checkbox } from "../../components/Checkbox";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Dialog } from "../../components/Dialog";
import { Badge, Card, EmptyState, ErrorState, LoadingState } from "../../components/Feedback";
import { Glyph } from "../../components/Glyph";
import { ContextMenu, Menu, type MenuEntry } from "../../components/Menu";
import { ProgressBar } from "../../components/ProgressBar";
import { SafeMarkdown } from "../../components/SafeMarkdown";
import { Select } from "../../components/Select";
import { Switch } from "../../components/Switch";
import { Tabs } from "../../components/Tabs";
import { TextArea, TextField } from "../../components/TextField";
import { useToast } from "../../components/Toast";
import { Tooltip } from "../../components/Tooltip";
import { t } from "../../i18n";
import { commands, type Theme } from "../../ipc";
import { queryKeys, unwrap } from "../../ipc/query";
import styles from "./Gallery.module.css";

const SAMPLE_MARKDOWN = `## About this package

A **co-op puzzle game** for two players. Features:

- Split-screen and online play
- Controller support

Visit [the official site](https://example.org/portal) or try [a bad link](javascript:alert(1)).

<script>alert("raw HTML is shown as text")</script>`;

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className={styles.section} aria-label={title}>
      <h2>{title}</h2>
      {children}
    </section>
  );
}

export function Gallery() {
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const appearance = useAppearanceQuery();
  const setTheme = useMutation({
    mutationFn: async (theme: Theme) =>
      unwrap(
        await commands.appearanceSet({
          theme,
          reduce_motion: appearance.data?.reduce_motion ?? false,
        }),
      ),
    onSuccess: (data) => queryClient.setQueryData(queryKeys.appearance, data),
  });
  const [text, setText] = useState("");
  const [area, setArea] = useState("");
  const [checked, setChecked] = useState(true);
  const [on, setOn] = useState(false);
  const [dialog, setDialog] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [typed, setTyped] = useState(false);
  const [tab, setTab] = useState<"general" | "advanced" | "about">("general");
  const [progress, setProgress] = useState(0.42);

  const entries: MenuEntry[] = [
    { id: "play", label: "Play", icon: "play", onSelect: () => toast({ title: "Play" }) },
    {
      id: "fav",
      label: "Add to favorites",
      icon: "star",
      onSelect: () => toast({ title: "Favorite" }),
    },
    { id: "disabled", label: "Verify files", icon: "shield", disabled: true, onSelect: () => {} },
    { id: "sep", separator: true },
    {
      id: "remove",
      label: "Uninstall…",
      icon: "trash",
      danger: true,
      onSelect: () => setConfirm(true),
    },
  ];

  const tabLabel = {
    general: t("gallery.tabGeneral"),
    advanced: t("gallery.tabAdvanced"),
    about: t("gallery.tabAbout"),
  };

  return (
    <main className={styles.page}>
      <header className={styles.intro}>
        <h1>{t("gallery.title")}</h1>
        <p>{t("gallery.description")}</p>
      </header>

      <Section title={t("gallery.buttons")}>
        <div className={styles.row}>
          <Button variant="primary" icon="play">
            Play
          </Button>
          <Button>Secondary</Button>
          <Button variant="ghost">Ghost</Button>
          <Button variant="danger" icon="trash">
            Uninstall
          </Button>
          <Button variant="primary" loading>
            Installing
          </Button>
          <Button disabled>Disabled</Button>
          <Button size="sm">Small</Button>
          <Button size="lg" variant="primary">
            Large
          </Button>
          <IconButton icon="settings" label="Settings" />
          <IconButton icon="star" label="Add to favorites" variant="secondary" />
          <Tooltip content="Tooltips also appear on keyboard focus." describe>
            <Button>With tooltip</Button>
          </Tooltip>
        </div>
      </Section>

      <Section title={t("gallery.fields")}>
        <div className={styles.grid}>
          <TextField
            label={t("gallery.serverAddress")}
            placeholder="https://games.example.org"
            value={text}
            onChange={(e) => setText(e.target.value)}
            description={t("gallery.sampleHint")}
          />
          <TextField
            label={t("gallery.sampleLabel")}
            defaultValue="Invalid value"
            error={t("gallery.sampleError")}
          />
          <TextField label={t("gallery.sampleLabel")} value="Read only" readOnly />
          <TextArea
            label="Message"
            value={area}
            onChange={(e) => setArea(e.target.value)}
            maxChars={40}
          />
          <Select
            label={t("gallery.themeLabel")}
            value={appearance.data?.theme ?? "dark"}
            onChange={(theme) => setTheme.mutate(theme)}
            options={[
              { value: "dark", label: t("gallery.themeDark") },
              { value: "light", label: t("gallery.themeLight") },
              { value: "high_contrast", label: t("gallery.themeHighContrast") },
              { value: "system", label: t("gallery.themeSystem") },
            ]}
          />
          <Select
            label="Disabled option"
            value={null}
            placeholder="Choose…"
            onChange={() => {}}
            options={[
              { value: "a", label: "Available" },
              { value: "b", label: "Unavailable", disabled: true },
              { value: "c", label: "Also available" },
            ]}
          />
        </div>
      </Section>

      <Section title={t("gallery.choices")}>
        <div className={styles.grid}>
          <Checkbox
            label={t("gallery.notifyMe")}
            checked={checked}
            onCheckedChange={setChecked}
            description={t("gallery.sampleHint")}
          />
          <Checkbox label="Disabled" disabled />
          <Switch label={t("gallery.compact")} checked={on} onCheckedChange={setOn} />
          <Switch label="Disabled switch" checked onCheckedChange={() => {}} disabled />
        </div>
      </Section>

      <Section title={t("gallery.overlays")}>
        <div className={styles.row}>
          <Button onClick={() => setDialog(true)}>{t("gallery.openDialog")}</Button>
          <Button variant="danger" onClick={() => setConfirm(true)}>
            {t("gallery.openConfirm")}
          </Button>
          <Button variant="danger" onClick={() => setTyped(true)}>
            {t("gallery.openTypedConfirm")}
          </Button>
          <Menu
            label={t("gallery.menuLabel")}
            entries={entries}
            trigger={<IconButton icon="more" label={t("gallery.menuLabel")} />}
          />
        </div>
        <ContextMenu label={t("gallery.contextCard")} entries={entries}>
          <Card raised className={styles.contextCard}>
            <strong>{t("gallery.contextCard")}</strong>
            <p>{t("gallery.contextHint")}</p>
            <div className={styles.row}>
              <Button variant="primary" icon="play">
                Play
              </Button>
            </div>
          </Card>
        </ContextMenu>
        <Dialog
          open={dialog}
          onClose={() => setDialog(false)}
          title={t("gallery.dialogTitle")}
          description={t("gallery.dialogBody")}
          footer={
            <>
              <Button onClick={() => setDialog(false)}>{t("common.cancel")}</Button>
              <Button variant="primary" onClick={() => setDialog(false)}>
                {t("common.done")}
              </Button>
            </>
          }
        >
          <TextField label={t("gallery.sampleLabel")} />
        </Dialog>
        <ConfirmDialog
          open={confirm}
          tone="danger"
          title={t("gallery.confirmTitle")}
          description={t("gallery.confirmBody")}
          confirmLabel={t("gallery.confirmAction")}
          onCancel={() => setConfirm(false)}
          onConfirm={() => setConfirm(false)}
        />
        <ConfirmDialog
          open={typed}
          tone="danger"
          title={t("gallery.typedTitle")}
          confirmLabel={t("gallery.confirmAction")}
          typedConfirmation="Portal 2"
          onCancel={() => setTyped(false)}
          onConfirm={async () => {
            await new Promise((resolve) => setTimeout(resolve, 800));
            setTyped(false);
          }}
        />
      </Section>

      <Section title={t("gallery.feedback")}>
        <div className={styles.row}>
          <Button
            onClick={() =>
              toast({ title: t("gallery.toastSample"), description: t("gallery.toastSampleText") })
            }
          >
            {t("gallery.toastInfo")}
          </Button>
          <Button
            onClick={() =>
              toast({
                tone: "success",
                title: t("gallery.toastSample"),
                action: { label: "Play", onClick: () => {} },
              })
            }
          >
            {t("gallery.toastSuccess")}
          </Button>
          <Button onClick={() => toast({ tone: "warning", title: "Library offline" })}>
            {t("gallery.toastWarning")}
          </Button>
          <Button
            onClick={() => toast({ tone: "danger", title: "Download failed", duration: null })}
          >
            {t("gallery.toastDanger")}
          </Button>
        </div>
        <div className={styles.row}>
          <Badge>Neutral</Badge>
          <Badge tone="accent" icon="download">
            Update available
          </Badge>
          <Badge tone="success" icon="play">
            Running
          </Badge>
          <Badge tone="warning" icon="warning">
            Incomplete
          </Badge>
          <Badge tone="danger" icon="offline">
            Library offline
          </Badge>
        </div>
        <div className={styles.grid}>
          <Card>
            <EmptyState
              icon="library"
              title={t("gallery.emptyTitle")}
              description={t("gallery.emptyText")}
              action={<Button variant="primary">Browse</Button>}
            />
          </Card>
          <Card>
            <ErrorState
              title={t("gallery.errorTitle")}
              description={t("gallery.errorText")}
              onRetry={() => {}}
              details="request_id=01J…"
            />
          </Card>
          <Card>
            <LoadingState />
          </Card>
        </div>
      </Section>

      <Section title={t("gallery.progress")}>
        <div className={styles.grid}>
          <ProgressBar
            label={t("gallery.progressDeterminate")}
            value={progress}
            valueText="1.7 GB of 4.0 GB"
          />
          <ProgressBar label={t("gallery.progressIndeterminate")} />
          <ProgressBar label="Failed" value={0.73} tone="danger" />
        </div>
        <div className={styles.row}>
          <Button size="sm" onClick={() => setProgress((p) => Math.max(0, p - 0.1))}>
            −10%
          </Button>
          <Button size="sm" onClick={() => setProgress((p) => Math.min(1, p + 0.1))}>
            +10%
          </Button>
        </div>
      </Section>

      <Section title={t("gallery.tabs")}>
        <Tabs
          label={t("gallery.tabs")}
          value={tab}
          onChange={setTab}
          items={[
            { value: "general", label: tabLabel.general },
            { value: "advanced", label: tabLabel.advanced },
            { value: "about", label: tabLabel.about },
          ]}
        >
          <p>{t("gallery.tabPanel", { tab: tabLabel[tab] })}</p>
        </Tabs>
      </Section>

      <Section title={t("gallery.markdown")}>
        <Card>
          <SafeMarkdown source={SAMPLE_MARKDOWN} />
        </Card>
      </Section>

      <Section title={t("gallery.glyphs")}>
        <div className={styles.row}>
          <Glyph action="accept" /> {t("nav.select")}
          <Glyph action="back" /> {t("nav.back")}
          <Glyph action="menu" /> {t("nav.options")}
          <Glyph action="tab_prev" />
          <Glyph action="tab_next" /> {t("nav.switchTab")}
        </div>
      </Section>
    </main>
  );
}

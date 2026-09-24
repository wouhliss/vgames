// The only way to show formatted text that came from a server (package descriptions, MOTDs,
// release notes written by admins). It parses CommonMark into a syntax tree and renders a fixed set
// of node types as React elements:
//
// - Raw HTML is never interpreted: HTML nodes are shown as the literal text they contain.
// - Images are not loaded (the CSP only allows vgimg: anyway); their alt text is shown.
// - Links become buttons that ask for confirmation, then open http(s) URLs in the system browser
//   through a Rust command. Any other scheme (javascript:, file:, data:, vgames:) renders as text.
//
// The parser is loaded on demand so it stays out of the initial bundle; until then the source is
// shown as plain text.
import type { Nodes, Root, RootContent } from "mdast";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { t } from "../i18n";
import { commands } from "../ipc";
import { ConfirmDialog } from "./ConfirmDialog";
import { Icon } from "./Icon";
import styles from "./SafeMarkdown.module.css";

type Parser = (source: string) => Root;

const MAX_SOURCE_LENGTH = 100_000;
const MAX_DEPTH = 24;

let parser: Parser | null = null;
let parserPromise: Promise<Parser> | null = null;

function loadParser(): Promise<Parser> {
  parserPromise ??= import("mdast-util-from-markdown").then((m) => {
    parser = (source) => m.fromMarkdown(source);
    return parser;
  });
  return parserPromise;
}

/** Absolute http(s) URL, or null. */
export function safeHref(url: string): string | null {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" || parsed.protocol === "http:" ? parsed.href : null;
  } catch {
    return null;
  }
}

function textOf(node: Nodes): string {
  if ("value" in node && typeof node.value === "string") return node.value;
  if ("alt" in node && typeof node.alt === "string") return node.alt;
  if ("children" in node) return (node.children as Nodes[]).map(textOf).join("");
  return "";
}

interface RenderContext {
  headingOffset: number;
  definitions: Map<string, string>;
  openLink: (href: string) => void;
}

function renderChildren(
  nodes: readonly RootContent[],
  ctx: RenderContext,
  depth: number,
): ReactNode[] {
  return nodes.map((child, i) => renderNode(child, ctx, depth + 1, i));
}

function renderLink(
  href: string | null,
  children: ReactNode,
  key: number,
  ctx: RenderContext,
): ReactNode {
  if (!href) return <span key={key}>{children}</span>;
  return (
    <button
      key={key}
      type="button"
      className={styles.link}
      title={href}
      onClick={() => ctx.openLink(href)}
    >
      {children}
      <span className={styles.linkIcon}>
        <Icon name="external" size={14} />
      </span>
      <span className="visually-hidden"> {t("markdown.opensInBrowser")}</span>
    </button>
  );
}

function renderNode(node: RootContent, ctx: RenderContext, depth: number, key: number): ReactNode {
  if (depth > MAX_DEPTH) return <span key={key}>{textOf(node)}</span>;
  switch (node.type) {
    case "paragraph":
      return <p key={key}>{renderChildren(node.children, ctx, depth)}</p>;
    case "heading": {
      const level = Math.min(6, node.depth + ctx.headingOffset);
      const Tag = `h${level}` as "h3";
      return <Tag key={key}>{renderChildren(node.children, ctx, depth)}</Tag>;
    }
    case "text":
      return node.value;
    case "emphasis":
      return <em key={key}>{renderChildren(node.children, ctx, depth)}</em>;
    case "strong":
      return <strong key={key}>{renderChildren(node.children, ctx, depth)}</strong>;
    case "inlineCode":
      return <code key={key}>{node.value}</code>;
    case "code":
      return (
        <pre key={key}>
          <code>{node.value}</code>
        </pre>
      );
    case "blockquote":
      return <blockquote key={key}>{renderChildren(node.children, ctx, depth)}</blockquote>;
    case "list": {
      const items = renderChildren(node.children, ctx, depth);
      return node.ordered ? (
        <ol key={key} start={node.start ?? undefined}>
          {items}
        </ol>
      ) : (
        <ul key={key}>{items}</ul>
      );
    }
    case "listItem":
      // Tight list items hold paragraphs; unwrap them so bullets do not get paragraph spacing.
      return (
        <li key={key}>
          {node.spread
            ? renderChildren(node.children, ctx, depth)
            : node.children.map((child, i) =>
                child.type === "paragraph" ? (
                  // biome-ignore lint/suspicious/noArrayIndexKey: the tree is immutable per render; position is identity.
                  <span key={i}>{renderChildren(child.children, ctx, depth + 1)}</span>
                ) : (
                  renderNode(child, ctx, depth + 1, i)
                ),
              )}
        </li>
      );
    case "thematicBreak":
      return <hr key={key} />;
    case "break":
      return <br key={key} />;
    case "link":
      return renderLink(safeHref(node.url), renderChildren(node.children, ctx, depth), key, ctx);
    case "linkReference": {
      const url = ctx.definitions.get(node.identifier);
      return renderLink(
        url ? safeHref(url) : null,
        renderChildren(node.children, ctx, depth),
        key,
        ctx,
      );
    }
    case "image":
    case "imageReference":
      return node.alt ? <span key={key}>[{node.alt}]</span> : null;
    case "html":
      // Raw HTML is shown as text, never parsed.
      return <span key={key}>{node.value}</span>;
    case "definition":
      return null;
    default:
      return <span key={key}>{textOf(node)}</span>;
  }
}

export interface SafeMarkdownProps {
  source: string;
  /** Added to heading depth so `# Title` in a description does not compete with page headings. */
  headingOffset?: number;
  className?: string;
}

export function SafeMarkdown({ source, headingOffset = 2, className }: SafeMarkdownProps) {
  const [ready, setReady] = useState<Parser | null>(() => parser);
  const [pendingUrl, setPendingUrl] = useState<string | null>(null);
  const [openError, setOpenError] = useState<string | null>(null);
  const text = source.length > MAX_SOURCE_LENGTH ? source.slice(0, MAX_SOURCE_LENGTH) : source;

  useEffect(() => {
    if (ready) return;
    let alive = true;
    loadParser()
      .then((p) => {
        if (alive) setReady(() => p);
      })
      .catch(() => {
        // Keep showing plain text.
      });
    return () => {
      alive = false;
    };
  }, [ready]);

  const tree = useMemo(() => (ready ? ready(text) : null), [ready, text]);
  const classes = [styles.markdown, className].filter(Boolean).join(" ");

  const dialog = (
    <ConfirmDialog
      open={pendingUrl !== null}
      title={t("markdown.openLinkTitle")}
      description={t("markdown.openLinkDescription")}
      confirmLabel={t("markdown.openLinkConfirm")}
      onCancel={() => {
        setPendingUrl(null);
        setOpenError(null);
      }}
      onConfirm={async () => {
        if (!pendingUrl) return;
        const result = await commands.openExternalUrl(pendingUrl);
        if (result.status === "ok") {
          setPendingUrl(null);
          setOpenError(null);
        } else {
          setOpenError(t("markdown.openLinkFailed"));
        }
      }}
    >
      <code className={styles.url}>{pendingUrl}</code>
      {openError ? (
        <p className={styles.error} role="alert">
          {openError}
        </p>
      ) : null}
    </ConfirmDialog>
  );

  if (!tree) {
    return (
      <div className={classes}>
        <p className={styles.plain}>{text}</p>
      </div>
    );
  }

  const definitions = new Map<string, string>();
  for (const node of tree.children) {
    if (node.type === "definition") definitions.set(node.identifier, node.url);
  }
  const ctx: RenderContext = { headingOffset, definitions, openLink: setPendingUrl };

  return (
    <div className={classes}>
      {renderChildren(tree.children, ctx, 0)}
      {dialog}
    </div>
  );
}

/** Test hook: resolves once the parser chunk has loaded. */
export function preloadMarkdownParser(): Promise<unknown> {
  return loadParser();
}

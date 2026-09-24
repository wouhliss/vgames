import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeAll, describe, expect, it } from "vitest";
import { installMockBackend } from "../mocks/backend";
import { renderWithProviders } from "../test/render";
import { preloadMarkdownParser, SafeMarkdown, safeHref } from "./SafeMarkdown";

beforeAll(async () => {
  await preloadMarkdownParser();
});

describe("SafeMarkdown", () => {
  it("renders CommonMark structure", () => {
    const { container } = renderWithProviders(
      <SafeMarkdown source={"# Title\n\nSome **bold** and *em* text.\n\n- one\n- two\n\n`code`"} />,
    );
    expect(screen.getByRole("heading", { level: 3, name: "Title" })).toBeInTheDocument();
    expect(container.querySelector("strong")).toHaveTextContent("bold");
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });

  it("never interprets raw HTML", () => {
    const source =
      '<script>alert(1)</script>\n\n<img src=x onerror="alert(2)">\n\nHello <b>there</b>';
    const { container } = renderWithProviders(<SafeMarkdown source={source} />);
    expect(container.querySelector("script, img, b, iframe")).toBeNull();
    expect(container).toHaveTextContent("<script>alert(1)</script>");
    expect(container).toHaveTextContent("<b>there</b>");
  });

  it("does not make unsafe URLs clickable and never loads images", () => {
    const source =
      "[x](javascript:alert(1)) [y](file:///etc/passwd) [z](vgames://launch/1) ![logo](https://evil.example/p.png)";
    const { container } = renderWithProviders(<SafeMarkdown source={source} />);
    expect(screen.queryByRole("button")).toBeNull();
    expect(container.querySelector("a, img")).toBeNull();
    expect(container).toHaveTextContent("[logo]");
  });

  it("asks before opening http(s) links in the browser", async () => {
    const user = userEvent.setup();
    const backend = installMockBackend();
    renderWithProviders(
      <SafeMarkdown
        source={
          "See [the site](https://example.org/page) and [ref][1].\n\n[1]: https://example.org/ref"
        }
      />,
    );
    await user.click(screen.getByRole("button", { name: /the site/ }));
    expect(
      screen.getByRole("alertdialog", { name: "Open this link in your browser?" }),
    ).toHaveTextContent("https://example.org/page");
    expect(backend.callsTo("open_external_url")).toHaveLength(0);
    await user.click(screen.getByRole("button", { name: "Open link" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(backend.callsTo("open_external_url")[0]?.args).toEqual({
      url: "https://example.org/page",
    });
    expect(screen.getByRole("button", { name: /ref/ })).toBeInTheDocument();
  });

  it("validates schemes", () => {
    expect(safeHref("https://example.org")).toBe("https://example.org/");
    expect(safeHref("javascript:alert(1)")).toBeNull();
    expect(safeHref("//example.org")).toBeNull();
    expect(safeHref("data:text/html,x")).toBeNull();
  });
});

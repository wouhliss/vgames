import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Tooltip } from "./Tooltip";

afterEach(() => {
  vi.restoreAllMocks();
});

/** jsdom has no layout: the panel spans x 120–1120 in a 1280 px window; the tooltip spans `tip`. */
function renderInPanel(tip: [number, number] = [500, 626]) {
  const place = { tip };
  vi.spyOn(document.documentElement, "clientWidth", "get").mockReturnValue(1280);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement,
  ) {
    const [left, right] = this.getAttribute("role") === "tooltip" ? place.tip : [120, 1120];
    return {
      left,
      right,
      top: 80,
      bottom: 680,
      width: right - left,
      height: 600,
      x: left,
      y: 80,
      toJSON: () => ({}),
    } as DOMRect;
  });
  render(
    <div style={{ overflow: "auto" }}>
      <Tooltip content="Previous screenshot">
        <button type="button">‹</button>
      </Tooltip>
    </div>,
  );
  document.documentElement.dataset.modality = "keyboard";
  return place;
}

describe("Tooltip", () => {
  it("shows on keyboard focus and hides on Escape", async () => {
    renderInPanel();
    fireEvent.focus(screen.getByRole("button"));
    expect(await screen.findByRole("tooltip", { hidden: true })).toHaveTextContent(
      "Previous screenshot",
    );
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("tooltip", { hidden: true })).not.toBeInTheDocument();
  });

  it("moves sideways to stay inside a scrolling panel", async () => {
    // Centered under a button at the panel's left edge, the tooltip would start at x = 110.
    renderInPanel([110, 236]);
    fireEvent.focus(screen.getByRole("button"));
    // 120 (panel edge) + 4 (margin) - 110 = 14 px to the right.
    expect((await screen.findByRole("tooltip", { hidden: true })).style.translate).toBe(
      "calc(-50% + 14px) 0",
    );
  });

  it("moves left at the window's right edge", async () => {
    renderInPanel([1200, 1300]);
    fireEvent.focus(screen.getByRole("button"));
    // The panel ends at 1120: 1116 - 1300 = -184 px.
    expect((await screen.findByRole("tooltip", { hidden: true })).style.translate).toBe(
      "calc(-50% + -184px) 0",
    );
  });

  it("forgets the shift when it closes", async () => {
    const place = renderInPanel([110, 236]);
    const button = screen.getByRole("button");
    fireEvent.focus(button);
    expect((await screen.findByRole("tooltip", { hidden: true })).style.translate).not.toBe("");
    fireEvent.blur(button);
    place.tip = [500, 626];
    fireEvent.focus(button);
    expect((await screen.findByRole("tooltip", { hidden: true })).style.translate).toBe("");
  });

  it("stays centered when it fits", async () => {
    renderInPanel();
    fireEvent.focus(screen.getByRole("button"));
    expect((await screen.findByRole("tooltip", { hidden: true })).style.translate).toBe("");
  });
});

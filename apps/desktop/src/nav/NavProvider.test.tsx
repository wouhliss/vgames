import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { Button } from "../components/Button";
import { Dialog } from "../components/Dialog";
import { Tabs } from "../components/Tabs";
import { events, type NavAction } from "../ipc";
import { installMockBackend } from "../mocks/backend";
import { flush, renderWithProviders, setRect } from "../test/render";
import { useBackHandler } from "./NavProvider";

async function pad(action: NavAction) {
  await act(async () => {
    await events.uiNav.emit({ action, controller: "dualsense", repeat: false });
    await flush();
  });
}

function Grid({ onActivate }: { onActivate: (name: string) => void }) {
  return (
    <div>
      {["a", "b", "c", "d"].map((name) => (
        <Button key={name} onClick={() => onActivate(name)}>
          {name}
        </Button>
      ))}
    </div>
  );
}

function layoutGrid() {
  // a b
  // c d
  const [a, b, c, d] = ["a", "b", "c", "d"].map((n) => screen.getByRole("button", { name: n }));
  setRect(a as Element, 0, 0, 100, 50);
  setRect(b as Element, 120, 0, 100, 50);
  setRect(c as Element, 0, 70, 100, 50);
  setRect(d as Element, 120, 70, 100, 50);
  return { a: a as HTMLElement, b: b as HTMLElement, c: c as HTMLElement, d: d as HTMLElement };
}

describe("spatial navigation", () => {
  beforeEach(() => {
    installMockBackend();
  });

  it("moves focus geometrically with arrow keys and marks keyboard modality", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Grid onActivate={() => {}} />);
    const { a, b, c, d } = layoutGrid();
    a.focus();
    await user.keyboard("{ArrowRight}");
    expect(b).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(d).toHaveFocus();
    await user.keyboard("{ArrowLeft}");
    expect(c).toHaveFocus();
    expect(document.documentElement.dataset.modality).toBe("keyboard");
  });

  it("keeps arrow keys inside a text field until the caret reaches the edge", async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <div>
        <input aria-label="Name" defaultValue="ab" />
        <Button>next</Button>
      </div>,
    );
    const input = screen.getByRole<HTMLInputElement>("textbox", { name: "Name" });
    const next = screen.getByRole("button", { name: "next" });
    setRect(input, 0, 0, 100, 30);
    setRect(next, 120, 0, 60, 30);
    await user.click(input);
    input.setSelectionRange(1, 1);
    await user.keyboard("{ArrowRight}");
    expect(input).toHaveFocus();
    await user.keyboard("{ArrowRight}");
    expect(next).toHaveFocus();
  });

  it("drives focus and activation from controller events", async () => {
    const onActivate = vi.fn();
    renderWithProviders(<Grid onActivate={onActivate} />);
    await flush();
    const { a, b, d } = layoutGrid();
    a.focus();
    await pad("right");
    expect(b).toHaveFocus();
    await pad("down");
    expect(d).toHaveFocus();
    expect(document.documentElement.dataset.modality).toBe("gamepad");
    await pad("accept");
    expect(onActivate).toHaveBeenCalledWith("d");
  });

  it("closes the top dialog with B and keeps D-pad movement inside it", async () => {
    function WithDialog() {
      const [open, setOpen] = useState(true);
      return (
        <>
          <Button>outside</Button>
          <Dialog open={open} onClose={() => setOpen(false)} title="Dialog">
            <Button>inside</Button>
          </Dialog>
        </>
      );
    }
    renderWithProviders(<WithDialog />);
    await flush();
    setRect(screen.getByRole("button", { name: "outside" }), 0, 300, 100, 40);
    setRect(screen.getByRole("button", { name: "Close" }), 400, 0, 40, 40);
    setRect(screen.getByRole("button", { name: "inside" }), 0, 60, 100, 40);
    screen.getByRole("button", { name: "inside" }).focus();
    await pad("down");
    expect(screen.getByRole("button", { name: "inside" })).toHaveFocus();
    await pad("back");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("switches tabs with the shoulder buttons", async () => {
    function WithTabs() {
      const [tab, setTab] = useState<"x" | "y">("x");
      return (
        <Tabs
          label="T"
          value={tab}
          onChange={setTab}
          items={[
            { value: "x", label: "X" },
            { value: "y", label: "Y" },
          ]}
        >
          <Button>panel</Button>
        </Tabs>
      );
    }
    renderWithProviders(<WithTabs />);
    await flush();
    await pad("tab_next");
    expect(screen.getByRole("tab", { name: "Y" })).toHaveAttribute("aria-selected", "true");
    await pad("tab_prev");
    expect(screen.getByRole("tab", { name: "X" })).toHaveAttribute("aria-selected", "true");
  });

  it("runs the registered back handler when nothing else consumed Escape", async () => {
    const onBack = vi.fn(() => true);
    function Page() {
      useBackHandler(onBack);
      return <Button>page</Button>;
    }
    const user = userEvent.setup();
    renderWithProviders(<Page />);
    screen.getByRole("button", { name: "page" }).focus();
    await user.keyboard("{Escape}");
    expect(onBack).toHaveBeenCalledTimes(1);
    await pad("back");
    expect(onBack).toHaveBeenCalledTimes(2);
  });
});

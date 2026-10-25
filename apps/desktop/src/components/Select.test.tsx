import { act, fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { events } from "../ipc";
import { installMockBackend } from "../mocks/backend";
import { flush, renderWithProviders, setRect } from "../test/render";
import { Button } from "./Button";
import { Dialog } from "./Dialog";
import { Select } from "./Select";

function Harness() {
  const [value, setValue] = useState<"dark" | "light" | "contrast" | "system">("dark");
  return (
    <Select
      label="Theme"
      value={value}
      onChange={setValue}
      options={[
        { value: "dark", label: "Dark" },
        { value: "light", label: "Light", disabled: true },
        { value: "contrast", label: "High contrast" },
        { value: "system", label: "System" },
      ]}
    />
  );
}

describe("Select", () => {
  it("is a named combobox showing the value", () => {
    renderWithProviders(<Harness />);
    const combo = screen.getByRole("combobox", { name: /Theme/ });
    expect(combo).toHaveTextContent("Dark");
    expect(combo).toHaveAttribute("aria-expanded", "false");
  });

  it("ignores queued scroll events but closes when its anchor moves", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    const combo = screen.getByRole("combobox", { name: /Theme/ });
    setRect(combo, 10, 50, 200, 36);
    await user.click(combo);
    expect(screen.getByRole("listbox", { name: "Theme" })).toBeVisible();
    fireEvent.scroll(window);
    expect(screen.getByRole("listbox", { name: "Theme" })).toBeVisible();
    setRect(combo, 10, 20, 200, 36);
    fireEvent.scroll(window);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("opens with ArrowDown, skips disabled options, selects with Enter", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    const combo = screen.getByRole("combobox", { name: /Theme/ });
    combo.focus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("listbox", { name: "Theme" })).toBeInTheDocument();
    expect(combo).toHaveAttribute("aria-activedescendant", expect.stringContaining("opt-0"));
    await user.keyboard("{ArrowDown}");
    expect(combo).toHaveAttribute("aria-activedescendant", expect.stringContaining("opt-2"));
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(combo).toHaveTextContent("High contrast");
    expect(combo).toHaveFocus();
  });

  it("leaves the D-pad to move past a closed list; A opens it", async () => {
    installMockBackend();
    renderWithProviders(
      <div>
        <Harness />
        <Button>Below</Button>
      </div>,
    );
    const combo = screen.getByRole("combobox", { name: /Theme/ });
    const below = screen.getByRole("button", { name: "Below" });
    setRect(combo, 0, 0, 200, 36);
    setRect(below, 0, 60, 200, 36);
    const pad = (action: "down" | "up" | "accept") =>
      act(async () => {
        await events.uiNav.emit({ action, controller: "dualsense", repeat: false });
        await flush();
      });
    combo.focus();
    await pad("down");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(below).toHaveFocus();
    await pad("up");
    expect(combo).toHaveFocus();
    await pad("accept");
    expect(screen.getByRole("listbox", { name: "Theme" })).toBeInTheDocument();
    await pad("down");
    expect(combo).toHaveAttribute("aria-activedescendant", expect.stringContaining("opt-2"));
  });

  it("closes on Escape without changing the value", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    const combo = screen.getByRole("combobox", { name: /Theme/ });
    combo.focus();
    await user.keyboard("{Enter}{End}{Escape}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(combo).toHaveTextContent("Dark");
  });

  it("selects by clicking an option", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    await user.click(screen.getByRole("combobox", { name: /Theme/ }));
    await user.click(screen.getByRole("option", { name: "System" }));
    expect(screen.getByRole("combobox", { name: /Theme/ })).toHaveTextContent("System");
  });

  it("inside a dialog, opens in the dialog's layer, and Escape closes only the list", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    renderWithProviders(
      <Dialog open title="Settings" onClose={onClose}>
        <Harness />
      </Dialog>,
    );
    const combo = screen.getByRole("combobox", { name: /Theme/ });
    await user.click(combo);
    // Outside the layer it would stack under the dialog (and be made inert with the rest of <body>).
    const layer = screen.getByRole("dialog").closest("[data-modal-layer]");
    expect(layer).not.toBeNull();
    expect(layer?.contains(screen.getByRole("listbox"))).toBe(true);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

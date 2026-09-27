import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { renderWithProviders } from "../test/render";
import { Button, IconButton } from "./Button";
import { Dialog } from "./Dialog";
import { ContextMenu, Menu, type MenuEntry } from "./Menu";

function entries(onPlay = vi.fn(), onRemove = vi.fn()): MenuEntry[] {
  return [
    { id: "play", label: "Play", onSelect: onPlay },
    { id: "verify", label: "Verify", disabled: true, onSelect: vi.fn() },
    { id: "sep", separator: true },
    { id: "remove", label: "Uninstall", danger: true, onSelect: onRemove },
  ];
}

describe("Menu", () => {
  it("opens from the keyboard and focuses the first item", async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <Menu
        label="Actions"
        entries={entries()}
        trigger={<IconButton icon="more" label="More" />}
      />,
    );
    const trigger = screen.getByRole("button", { name: "More" });
    expect(trigger).toHaveAttribute("aria-haspopup", "menu");
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    trigger.focus();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("menu", { name: "Actions" })).toBeInTheDocument();
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByRole("menuitem", { name: "Play" })).toHaveFocus();
  });

  it("skips disabled items, wraps, and closes on Escape returning focus", async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <Menu
        label="Actions"
        entries={entries()}
        trigger={<IconButton icon="more" label="More" />}
      />,
    );
    const trigger = screen.getByRole("button", { name: "More" });
    trigger.focus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("menuitem", { name: "Play" })).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("menuitem", { name: "Uninstall" })).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("menuitem", { name: "Play" })).toHaveFocus();
    await user.keyboard("{End}");
    expect(screen.getByRole("menuitem", { name: "Uninstall" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("runs the selected item and ignores disabled ones", async () => {
    const user = userEvent.setup();
    const onPlay = vi.fn();
    renderWithProviders(
      <Menu
        label="Actions"
        entries={entries(onPlay)}
        trigger={<IconButton icon="more" label="More" />}
      />,
    );
    await user.click(screen.getByRole("button", { name: "More" }));
    await user.click(screen.getByRole("menuitem", { name: "Verify" }));
    expect(screen.getByRole("menu")).toBeInTheDocument();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("menu")).toBeInTheDocument();
    await user.keyboard("{Home}{Enter}");
    expect(onPlay).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("supports typeahead", async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <Menu
        label="Actions"
        entries={entries()}
        trigger={<IconButton icon="more" label="More" />}
      />,
    );
    await user.click(screen.getByRole("button", { name: "More" }));
    await user.keyboard("u");
    expect(screen.getByRole("menuitem", { name: "Uninstall" })).toHaveFocus();
  });
});

describe("ContextMenu", () => {
  it("opens with Shift+F10 and the ContextMenu key on the focused element", async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <ContextMenu label="Tile actions" entries={entries()}>
        <Button>Portal 2</Button>
      </ContextMenu>,
    );
    const tile = screen.getByRole("button", { name: "Portal 2" });
    tile.focus();
    await user.keyboard("{Shift>}{F10}{/Shift}");
    expect(screen.getByRole("menu", { name: "Tile actions" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(tile).toHaveFocus();
    await user.keyboard("{ContextMenu}");
    expect(screen.getByRole("menu", { name: "Tile actions" })).toBeInTheDocument();
  });

  it("opens on right-click", async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <ContextMenu label="Tile actions" entries={entries()}>
        <Button>Portal 2</Button>
      </ContextMenu>,
    );
    await user.pointer({
      keys: "[MouseRight]",
      target: screen.getByRole("button", { name: "Portal 2" }),
    });
    expect(screen.getByRole("menu", { name: "Tile actions" })).toBeInTheDocument();
  });

  it("inside a dialog, opens in the dialog's layer", async () => {
    const user = userEvent.setup();
    renderWithProviders(
      <Dialog open title="Game" onClose={vi.fn()}>
        <Menu
          label="Actions"
          entries={entries()}
          trigger={<IconButton icon="more" label="More" />}
        />
      </Dialog>,
    );
    await user.click(screen.getByRole("button", { name: "More" }));
    const layer = screen.getByRole("dialog").closest("[data-modal-layer]");
    expect(layer?.contains(screen.getByRole("menu", { name: "Actions" }))).toBe(true);
  });
});

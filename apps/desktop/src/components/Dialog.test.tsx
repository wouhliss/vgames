import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { renderWithProviders } from "../test/render";
import { Button } from "./Button";
import { ConfirmDialog } from "./ConfirmDialog";
import { Dialog } from "./Dialog";
import { TextField } from "./TextField";

function Harness({ dismissible = true }: { dismissible?: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button onClick={() => setOpen(true)}>Open</Button>
      <Button>Behind</Button>
      <Dialog
        open={open}
        onClose={() => setOpen(false)}
        title="Rename collection"
        description="Pick a new name."
        dismissible={dismissible}
        footer={<Button onClick={() => setOpen(false)}>Save</Button>}
      >
        <TextField label="Name" />
      </Dialog>
    </>
  );
}

describe("Dialog", () => {
  it("is a named modal dialog and moves focus inside", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    await user.click(screen.getByRole("button", { name: "Open" }));
    const dialog = screen.getByRole("dialog", { name: "Rename collection" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(dialog).toHaveAccessibleDescription("Pick a new name.");
    expect(dialog).toContainElement(document.activeElement as HTMLElement);
  });

  it("traps Tab and Shift+Tab", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    await user.click(screen.getByRole("button", { name: "Open" }));
    const close = screen.getByRole("button", { name: "Close" });
    const save = screen.getByRole("button", { name: "Save" });
    const name = screen.getByRole("textbox", { name: "Name" });
    // Focus starts in the content, not on the close button.
    expect(name).toHaveFocus();
    await user.tab();
    expect(save).toHaveFocus();
    await user.tab();
    expect(close).toHaveFocus();
    await user.tab();
    expect(name).toHaveFocus();
    await user.tab({ shift: true });
    expect(close).toHaveFocus();
    await user.tab({ shift: true });
    expect(save).toHaveFocus();
  });

  it("makes the rest of the page inert", async () => {
    const user = userEvent.setup();
    const { container } = renderWithProviders(<Harness />);
    await user.click(screen.getByRole("button", { name: "Open" }));
    expect(container).toHaveAttribute("inert");
    await user.keyboard("{Escape}");
    expect(container).not.toHaveAttribute("inert");
  });

  it("closes on Escape and returns focus to the opener", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    const opener = screen.getByRole("button", { name: "Open" });
    await user.click(opener);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(opener).toHaveFocus();
  });

  it("ignores Escape when not dismissible", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness dismissible={false} />);
    await user.click(screen.getByRole("button", { name: "Open" }));
    await user.keyboard("{Escape}");
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Close" })).not.toBeInTheDocument();
  });
});

describe("ConfirmDialog", () => {
  it("focuses Cancel first for destructive actions", () => {
    renderWithProviders(
      <ConfirmDialog
        open
        tone="danger"
        title="Delete?"
        confirmLabel="Delete"
        onConfirm={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(screen.getByRole("alertdialog", { name: "Delete?" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
  });

  it("requires the typed confirmation before confirming", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    renderWithProviders(
      <ConfirmDialog
        open
        tone="danger"
        title="Uninstall?"
        confirmLabel="Uninstall"
        typedConfirmation="Portal 2"
        onConfirm={onConfirm}
        onCancel={() => {}}
      />,
    );
    const confirm = screen.getByRole("button", { name: "Uninstall" });
    const input = screen.getByRole("textbox", { name: "Type Portal 2 to confirm" });
    expect(input).toHaveFocus();
    expect(confirm).toHaveAttribute("aria-disabled", "true");
    await user.click(confirm);
    expect(onConfirm).not.toHaveBeenCalled();
    await user.type(input, "portal 2");
    expect(confirm).toHaveAttribute("aria-disabled", "true");
    await user.clear(input);
    await user.type(input, "Portal 2{Enter}");
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });
});

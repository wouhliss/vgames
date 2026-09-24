import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { renderWithProviders } from "../test/render";
import { Button, IconButton } from "./Button";
import { Checkbox } from "./Checkbox";
import { ProgressBar } from "./ProgressBar";
import { Switch } from "./Switch";
import { TextArea, TextField } from "./TextField";
import { useToast } from "./Toast";

describe("fields", () => {
  it("links label, description and error to the input", () => {
    renderWithProviders(
      <TextField label="Server address" description="Starts with https://" error="Invalid" />,
    );
    const input = screen.getByRole("textbox", { name: "Server address" });
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input).toHaveAccessibleDescription("Starts with https:// Invalid");
  });

  it("counts characters and flags overflow", async () => {
    function Harness() {
      const [v, setV] = useState("");
      return (
        <TextArea label="Message" value={v} onChange={(e) => setV(e.target.value)} maxChars={3} />
      );
    }
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    const area = screen.getByRole("textbox", { name: "Message" });
    await user.type(area, "abcd");
    expect(area).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("4 / 3")).toBeInTheDocument();
  });

  it("toggles checkbox and switch by keyboard", async () => {
    function Harness() {
      const [a, setA] = useState(false);
      const [b, setB] = useState(false);
      return (
        <>
          <Checkbox label="Notify" checked={a} onCheckedChange={setA} />
          <Switch label="Compact" checked={b} onCheckedChange={setB} />
        </>
      );
    }
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    await user.tab();
    await user.keyboard(" ");
    expect(screen.getByRole("checkbox", { name: "Notify" })).toBeChecked();
    await user.tab();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("switch", { name: "Compact" })).toHaveAttribute("aria-checked", "true");
  });

  it("gives icon buttons an accessible name and keeps loading buttons focusable but inert", async () => {
    const onClick = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(
      <>
        <IconButton icon="settings" label="Settings" />
        <Button loading onClick={onClick}>
          Save
        </Button>
      </>,
    );
    expect(screen.getByRole("button", { name: "Settings" })).toBeInTheDocument();
    const save = screen.getByRole("button", { name: /Save/ });
    expect(save).toHaveAttribute("aria-busy", "true");
    await user.click(save);
    expect(onClick).not.toHaveBeenCalled();
    save.focus();
    expect(save).toHaveFocus();
  });

  it("exposes progress values", () => {
    renderWithProviders(
      <>
        <ProgressBar label="Downloading" value={0.425} valueText="1.7 GB of 4 GB" />
        <ProgressBar label="Verifying" />
      </>,
    );
    const determinate = screen.getByRole("progressbar", { name: "Downloading" });
    expect(determinate).toHaveAttribute("aria-valuenow", "43");
    expect(determinate).toHaveAttribute("aria-valuetext", "1.7 GB of 4 GB");
    expect(screen.getByRole("progressbar", { name: "Verifying" })).not.toHaveAttribute(
      "aria-valuenow",
    );
  });
});

describe("toasts", () => {
  it("announces in a live region, auto-dismisses, and can be dismissed", async () => {
    vi.useFakeTimers();
    let api: ReturnType<typeof useToast> | null = null;
    function Grab() {
      api = useToast();
      return null;
    }
    renderWithProviders(<Grab />);
    act(() => {
      api?.toast({ title: "Download finished", duration: 1000 });
      api?.toast({ title: "Disk full", tone: "danger", duration: null });
    });
    expect(screen.getByRole("status")).toHaveTextContent("Download finished");
    expect(screen.getByRole("alert")).toHaveTextContent("Disk full");
    act(() => {
      vi.advanceTimersByTime(1100);
    });
    expect(screen.queryByText("Download finished")).not.toBeInTheDocument();
    vi.useRealTimers();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Dismiss notification" }));
    expect(screen.queryByText("Disk full")).not.toBeInTheDocument();
  });
});

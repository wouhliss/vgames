import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { renderWithProviders } from "../test/render";
import { Button } from "./Button";
import { Tabs } from "./Tabs";

function Harness() {
  const [tab, setTab] = useState<"a" | "b" | "c">("a");
  return (
    <Tabs
      label="Sections"
      value={tab}
      onChange={setTab}
      items={[
        { value: "a", label: "General" },
        { value: "b", label: "Advanced" },
        { value: "c", label: "About" },
      ]}
    >
      <Button>Inside {tab}</Button>
    </Tabs>
  );
}

describe("Tabs", () => {
  it("exposes tablist, tabs and a labelled panel", () => {
    renderWithProviders(<Harness />);
    expect(screen.getByRole("tablist", { name: "Sections" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "General" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tabpanel", { name: "General" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Advanced" })).toHaveAttribute("tabindex", "-1");
  });

  it("moves with arrows, Home and End (automatic activation)", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    screen.getByRole("tab", { name: "General" }).focus();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Advanced" })).toHaveFocus();
    expect(screen.getByRole("tab", { name: "Advanced" })).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{End}");
    expect(screen.getByRole("tab", { name: "About" })).toHaveFocus();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "General" })).toHaveFocus();
    await user.keyboard("{Home}{ArrowLeft}");
    expect(screen.getByRole("tab", { name: "About" })).toHaveFocus();
  });

  it("switches with Ctrl+PageDown from inside the panel", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    screen.getByRole("button", { name: "Inside a" }).focus();
    await user.keyboard("{Control>}{PageDown}{/Control}");
    expect(screen.getByRole("tab", { name: "Advanced" })).toHaveAttribute("aria-selected", "true");
  });
});

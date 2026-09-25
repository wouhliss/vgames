import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { renderWithProviders } from "../test/render";
import { RadioGroup } from "./RadioGroup";

function Harness({ initial = "b" as "a" | "b" | "c" | "d" }) {
  const [value, setValue] = useState<"a" | "b" | "c" | "d">(initial);
  return (
    <>
      <RadioGroup
        label="Library"
        value={value}
        onChange={setValue}
        options={[
          { value: "a", label: "Games", description: "/home/sam/Games" },
          { value: "b", label: "SSD", aside: "120 GB free" },
          { value: "c", label: "External", disabled: true },
          { value: "d", label: "NAS" },
        ]}
      />
      <button type="button">After</button>
    </>
  );
}

describe("RadioGroup", () => {
  it("has radio semantics with one tab stop on the checked option", () => {
    renderWithProviders(<Harness />);
    const group = screen.getByRole("radiogroup", { name: "Library" });
    const radios = screen.getAllByRole("radio");
    expect(group).toContainElement(radios[0] ?? null);
    const ssd = screen.getByRole("radio", { name: "SSD" });
    expect(ssd).toHaveAttribute("aria-checked", "true");
    expect(ssd).toHaveAccessibleDescription("120 GB free");
    expect(radios.map((r) => r.tabIndex)).toEqual([-1, 0, -1, -1]);
    expect(screen.getByRole("radio", { name: /Games/ })).toHaveAccessibleDescription(
      "/home/sam/Games",
    );
    expect(screen.getByRole("radio", { name: /External/ })).toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("arrow keys move and select, skipping disabled options", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    await user.tab();
    expect(screen.getByRole("radio", { name: /SSD/ })).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    const nas = screen.getByRole("radio", { name: "NAS" });
    expect(nas).toHaveFocus();
    expect(nas).toHaveAttribute("aria-checked", "true");
    await user.keyboard("{ArrowUp}{ArrowUp}");
    expect(screen.getByRole("radio", { name: /Games/ })).toHaveAttribute("aria-checked", "true");
  });

  it("does not select disabled options on click", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness />);
    await user.click(screen.getByRole("radio", { name: /External/ }));
    expect(screen.getByRole("radio", { name: /SSD/ })).toHaveAttribute("aria-checked", "true");
  });

  it("leaves the key alone at the edges so spatial navigation can exit", async () => {
    const user = userEvent.setup();
    renderWithProviders(<Harness initial="d" />);
    await user.tab();
    const nas = screen.getByRole("radio", { name: "NAS" });
    expect(nas).toHaveFocus();
    const event = new KeyboardEvent("keydown", {
      key: "ArrowDown",
      bubbles: true,
      cancelable: true,
    });
    nas.dispatchEvent(event);
    // Not consumed by the group: the navigation layer may move focus onwards (jsdom has no layout,
    // so here nothing lies below and the event stays untouched).
    expect(event.defaultPrevented).toBe(false);
    expect(nas).toHaveAttribute("aria-checked", "true");
  });
});

import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { renderWithProviders } from "../test/render";
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
});

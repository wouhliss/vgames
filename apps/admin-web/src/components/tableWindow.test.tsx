// A3-T18: 10k-row lists stay light: only the rows near the scroll position are in the DOM.
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { manyPackages } from "../mocks/db";
import { renderAt } from "../test/render";
import { SpacerRow, THRESHOLD, useTableWindow } from "./tableWindow";

function Table({ count }: { count: number }) {
  const items = Array.from({ length: count }, (_, i) => ({ id: `row-${i}`, label: `Row ${i}` }));
  const win = useTableWindow(items);
  return (
    <div data-testid="wrap" ref={win.scrollRef} style={{ height: 600, overflow: "auto" }}>
      <table>
        <tbody>
          <SpacerRow height={win.before} columns={1} />
          {win.rows.map(({ item, index }) => (
            <tr key={item.id} ref={win.measure} data-index={index}>
              <td>{item.label}</td>
            </tr>
          ))}
          <SpacerRow height={win.after} columns={1} />
        </tbody>
      </table>
    </div>
  );
}

// jsdom has no layout: give scroll containers and rows the sizes a browser would (the virtualizer
// reads offsetWidth/offsetHeight).
const size = (el: HTMLElement) =>
  el.tagName === "TR" ? 37 : el.matches(".table-wrap, [data-testid=wrap]") ? 600 : 0;
const real = {
  height: Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight"),
  width: Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetWidth"),
};
beforeEach(() => {
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get(this: HTMLElement) {
      return size(this);
    },
  });
  Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
    configurable: true,
    get: () => 800,
  });
});
afterEach(() => {
  if (real.height) Object.defineProperty(HTMLElement.prototype, "offsetHeight", real.height);
  if (real.width) Object.defineProperty(HTMLElement.prototype, "offsetWidth", real.width);
});

const dataRows = () => screen.queryAllByRole("row").filter((r) => !r.classList.contains("spacer"));

describe("useTableWindow", () => {
  it("renders every row up to the threshold (find-in-page keeps working)", () => {
    render(<Table count={THRESHOLD} />);
    expect(dataRows()).toHaveLength(THRESHOLD);
    expect(document.querySelectorAll("tr.spacer")).toHaveLength(0);
  });

  it("keeps 10,000 rows down to a window, with the rest as space", () => {
    render(<Table count={10_000} />);
    const rows = dataRows();
    expect(rows.length).toBeGreaterThan(10);
    expect(rows.length).toBeLessThan(80);
    expect(rows[0]).toHaveTextContent("Row 0");
    const after = document.querySelector<HTMLElement>("tr.spacer");
    expect(Number.parseInt(after?.style.height ?? "0", 10)).toBeGreaterThan(300_000);
    // Space isn't a row to read out.
    expect(after).toHaveAttribute("role", "presentation");
  });

  it("follows the scroll position to the end of the list", async () => {
    render(<Table count={10_000} />);
    const wrap = screen.getByTestId("wrap");
    await act(async () => {
      wrap.scrollTop = 9_990 * 37;
      fireEvent.scroll(wrap);
    });
    expect(await screen.findByText("Row 9999")).toBeInTheDocument();
    expect(screen.queryByText("Row 0")).not.toBeInTheDocument();
    expect(dataRows().length).toBeLessThan(80);
  });
});

describe("list pages past the threshold", () => {
  it("packages: loading 250 of 10,000 keeps the table to a window", async () => {
    const { user } = renderAt("/packages", "admin", {
      db: (db) => (db.packages = manyPackages(10_000)),
    });
    for (let page = 1; page < 5; page++) {
      await user.click(await screen.findByRole("button", { name: "Load more" }));
      await screen.findByText(`${(page + 1) * 50} shown, more available`, { exact: false });
    }
    const rows = screen.getAllByRole("row").filter((r) => r.closest("tbody"));
    const data = rows.filter((r) => !r.classList.contains("spacer"));
    expect(data.length).toBeGreaterThan(10);
    expect(data.length).toBeLessThan(80);
    expect(screen.getByText("250 shown, more available", { exact: false })).toBeInTheDocument();
  });
});

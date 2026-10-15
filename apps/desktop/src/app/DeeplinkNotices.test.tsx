import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it } from "vitest";
import { type DeeplinkRefusalReason, events } from "../ipc";
import { installMockBackend } from "../mocks/backend";
import { flush, renderWithProviders } from "../test/render";
import { DeeplinkNotices } from "./DeeplinkNotices";

it.each([
  ["rate_limited", "Wait a moment before opening another game link."],
  ["not_installed", "Install this game before opening its launch link."],
  ["not_verified", "Verify this game's files before opening its launch link."],
  [
    "unknown_package",
    "This game link is unavailable on the selected server. Check that the correct server is selected.",
  ],
] satisfies [DeeplinkRefusalReason, string][])(
  "announces %s as fixed plain text",
  async (reason, message) => {
    installMockBackend();
    renderWithProviders(<DeeplinkNotices />);
    await flush();
    await act(async () => {
      await events.deeplinkRefused.emit({ reason });
    });
    expect(await screen.findByText(message)).toBeVisible();
    expect(document.querySelector("a, [href], script")).toBeNull();
  },
);

it("can be dismissed by keyboard and stops listening after unmount", async () => {
  installMockBackend();
  const user = userEvent.setup();
  const { unmount } = renderWithProviders(<DeeplinkNotices />);
  await flush();
  await act(async () => {
    await events.deeplinkRefused.emit({ reason: "not_verified" });
  });
  await screen.findByText("Game link could not open");
  await user.tab();
  const dismiss = screen.getByRole("button", { name: "Dismiss notification" });
  expect(dismiss).toHaveFocus();
  await user.keyboard("{Enter}");
  await waitFor(() => expect(screen.queryByText("Game link could not open")).toBeNull());
  unmount();
  await act(async () => {
    await events.deeplinkRefused.emit({ reason: "not_installed" });
  });
  expect(screen.queryByText("Game link could not open")).toBeNull();
});

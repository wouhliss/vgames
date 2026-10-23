import { axeScript, axeViolations } from "../../e2e/a11y.ts";
import type { Session } from "./webdriver.ts";

/** Instrument the actual release WebView through WebDriver, outside the product IPC. */
export async function nativeAxeViolations(session: Pick<Session, "execute" | "executeAsync">) {
  return axeViolations({
    async analyze(include) {
      await session.execute(axeScript());
      return session.executeAsync(
        `
        const done = arguments[arguments.length - 1];
        const scope = arguments[0] ? {include: [[arguments[0]]]} : document;
        axe.run(scope, {}, function(error, result) {
          done(error ? null : {violations: result.violations});
        });
      `,
        [include ?? null],
      );
    },
  });
}

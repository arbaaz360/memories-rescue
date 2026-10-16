import type { I18n } from "../i18n/l10n";
import links from "../config/links.json";

/**
 * Shown by a release build when the engine does not answer at startup: the
 * app never falls back to the mock there. Plain, without the store or the
 * backend, so it renders whatever else is broken.
 */
export function StartupError(props: { i18n: I18n; detail: string | null }) {
  const t = props.i18n.t;
  return (
    <main class="main startup-error">
      <div class="screen" role="alert">
        <h1 class="screen-title">{t("startup-error-title")}</h1>
        <p class="lede">{t("startup-error-body")}</p>
        <p>{t("startup-error-support", { email: links.support_email })}</p>
        {props.detail ? (
          <details class="error-detail">
            <summary>{t("error-detail-summary")}</summary>
            <p>
              <code>{props.detail}</code>
            </p>
          </details>
        ) : null}
      </div>
    </main>
  );
}

import { Rich, useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import { formatInstant } from "../format/format";
import { Button, Callout, ScreenTitle } from "../ui/components";
import { GuideArt, IconMail, IconPlay } from "../ui/icons";

/** S0: value statement, disclaimer, the FR-49 export guide, Resume previous job. */
export function Welcome() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const previous = store.resumable.value[0] ?? null;
  const support = store.appInfo.value?.support_email ?? "support@novabyteworks.com";

  return (
    <div class="screen screen-welcome">
      <ScreenTitle eyebrow={t("welcome-eyebrow")} lede={t("welcome-lede")}>
        {t("welcome-title")}
      </ScreenTitle>
      <ul class="value-list">
        <li>{t("welcome-value-dates")}</li>
        <li>{t("welcome-value-captions")}</li>
        <li>{t("welcome-value-library")}</li>
        <li>{t("welcome-value-private")}</li>
      </ul>

      <div class="hero-actions">
        <Button variant="primary" class="btn-lg" onClick={() => store.getStarted()}>
          {t("welcome-get-started")}
        </Button>
      </div>

      {previous ? (
        <section class="card resume-card" aria-labelledby="resume-title">
          <h2 id="resume-title" class="card-title">
            {t("welcome-resume-title")}
          </h2>
          <p>
            {t("welcome-resume-detail", {
              date: formatInstant(previous.created_at, i18n.formatLocale),
              done: previous.items_done,
              total: previous.items_total,
            })}
          </p>
          <p class="path">{previous.output_path}</p>
          <Button variant="secondary" onClick={() => void store.resumePrevious(previous)} busy={store.jobBusy.value}>
            <IconPlay size={18} />
            {t("welcome-resume")}
          </Button>
        </section>
      ) : null}

      <section class="guide" aria-labelledby="guide-title">
        <h2 id="guide-title">{t("guide-title")}</h2>
        <p>{t("guide-intro")}</p>
        <ol class="guide-steps">
          {([1, 2, 3, 4] as const).map((n) => (
            <li class="guide-step" key={n}>
              <GuideArt step={n} />
              <div>
                <h3 class="guide-step-title">
                  <span class="guide-step-num">{t("guide-step-number", { n })}</span>
                  {t(`guide-step${n}-title`)}
                </h3>
                <p>
                  <Rich id={`guide-step${n}-body`} />
                </p>
              </div>
            </li>
          ))}
        </ol>
      </section>

      <section class="notes" aria-labelledby="notes-title">
        <h2 id="notes-title">{t("notes-title")}</h2>
        <Callout tone="warning" title={t("note-expiry-title")}>
          <p>{t("note-expiry-body")}</p>
        </Callout>
        <Callout tone="info" title={t("note-meo-title")}>
          <p>{t("note-meo-body")}</p>
        </Callout>
        <Callout tone="info" title={t("note-2027-title")}>
          <p>{t("note-2027-body")}</p>
        </Callout>
      </section>

      <footer class="welcome-foot">
        <p>{t("welcome-not-affiliated")}</p>
        <p>
          {t("welcome-help")}{" "}
          <button type="button" class="link-btn" onClick={() => void store.host.openUrl(`mailto:${support}`)}>
            <IconMail size={16} />
            {support}
          </button>
        </p>
      </footer>
    </div>
  );
}

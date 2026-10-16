import type { ComponentChildren } from "preact";
import { useI18n } from "../i18n/context";
import { errorMessageId } from "../i18n/l10n";
import { useStore } from "../state/context";
import { formatNumber } from "../format/format";
import type { ItemStatus, JobReport } from "../ipc/types";
import { ActionBar, Badge, Button, Callout, ErrorCallout, Loading, ScreenTitle } from "../ui/components";
import { IconExternal, IconRefresh, IconKey, IconFile, IconPlay } from "../ui/icons";

function OutcomeRow(props: { status: ItemStatus; count: number; children?: ComponentChildren }) {
  const i18n = useI18n();
  const t = i18n.t;
  if (props.count === 0) return null;
  return (
    <li class={`outcome outcome-${props.status}`}>
      <span class="outcome-count">{formatNumber(props.count, i18n.formatLocale)}</span>
      <div class="outcome-text">
        <h4 class="outcome-label">{t("outcome-label", { status: props.status, count: props.count })}</h4>
        <p>{t("outcome-explain", { status: props.status, count: props.count })}</p>
        {props.children}
      </div>
    </li>
  );
}

function Groups(props: { report: JobReport }) {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const c = props.report.counts;
  const saved = c.done + c.metadata_partial + c.location_uncertain;
  const missingParts = props.report.missing_parts;
  return (
    <div class="outcome-groups">
      {saved > 0 ? (
        <section class="card outcome-group group-saved" aria-labelledby="g-saved">
          <h3 id="g-saved" class="card-title">
            {t("results-group-saved", { count: saved })}
          </h3>
          <ul class="outcomes">
            <OutcomeRow status="done" count={c.done} />
            <OutcomeRow status="metadata_partial" count={c.metadata_partial} />
            <OutcomeRow status="location_uncertain" count={c.location_uncertain}>
              <p class="field-desc">{t("results-uncertain-hint")}</p>
            </OutcomeRow>
          </ul>
        </section>
      ) : null}

      {c.missing_media > 0 ? (
        <section class="card outcome-group group-missing" aria-labelledby="g-missing">
          <h3 id="g-missing" class="card-title">
            {t("results-group-missing")}
          </h3>
          <ul class="outcomes">
            <OutcomeRow status="missing_media" count={c.missing_media}>
              {missingParts.length > 0 ? (
                <p>{t("results-missing-parts", { parts: new Intl.ListFormat(i18n.formatLocale).format(missingParts.map((n) => formatNumber(n, i18n.formatLocale))), count: missingParts.length })}</p>
              ) : null}
            </OutcomeRow>
          </ul>
          {props.report.can_add_part ? <Button onClick={() => void store.addMissingPartFromResults()}>{t("results-add-part")}</Button> : null}
        </section>
      ) : null}

      {c.skipped_free_limit > 0 ? (
        <section class="card outcome-group group-skipped" aria-labelledby="g-skipped">
          <h3 id="g-skipped" class="card-title">
            {t("results-group-skipped")}
          </h3>
          <ul class="outcomes">
            <OutcomeRow status="skipped_free_limit" count={c.skipped_free_limit} />
          </ul>
          {props.report.can_unlock ? (
            <Button variant="primary" onClick={() => store.openUnlock("results")}>
              <IconKey size={18} />
              {t("results-unlock")}
            </Button>
          ) : null}
        </section>
      ) : null}

      {c.failed > 0 ? (
        <section class="card outcome-group group-failed" aria-labelledby="g-failed">
          <h3 id="g-failed" class="card-title">
            {t("results-group-failed")}
          </h3>
          <ul class="outcomes">
            <OutcomeRow status="failed" count={c.failed} />
          </ul>
          <ul class="failure-list">
            {props.report.failures.slice(0, 10).map((f) => (
              <li key={f.item_id}>
                <span class="mono">{f.name}</span>
                <Badge tone="danger">{f.error_code ? t(errorMessageId(f.error_code)) : t("error-unknown")}</Badge>
              </li>
            ))}
          </ul>
          {props.report.can_retry ? (
            <Button variant="primary" onClick={() => void store.retryFailed()} busy={store.jobBusy.value}>
              <IconRefresh size={18} />
              {t("results-retry", { count: c.failed })}
            </Button>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}

/** FR-27: the split videos joined back together, and any that were not. */
function JoinedClips(props: { report: JobReport }) {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const clips = props.report.clips;
  if (clips.length === 0) return null;
  const done = clips.filter((c) => c.status === "done").length;
  return (
    <section class="card outcome-group group-saved" aria-labelledby="g-clips">
      <h3 id="g-clips" class="card-title">
        {t("results-clips-title", { count: done })}
      </h3>
      <ul class="clip-list">
        {clips.map((c) => (
          <li key={c.group_id}>
            <span>{t("results-clip", { count: c.clip_count, seconds: Math.round(c.total_seconds) })}</span>
            <Badge tone={c.status === "done" ? "success" : c.status === "failed" ? "danger" : "neutral"}>{t("results-clip-status", { status: c.status })}</Badge>
            {c.status === "done" && c.output_path ? (
              <Button variant="ghost" onClick={() => void store.openPath(c.output_path ?? "")}>
                {t("results-clip-open")}
              </Button>
            ) : null}
            {c.error_code ? <span class="muted">{t(errorMessageId(c.error_code))}</span> : null}
          </li>
        ))}
      </ul>
      <p class="field-desc">{t("results-clips-note")}</p>
    </section>
  );
}

/** S6: outcome by status, with "not in your export" kept apart from "failed" (NFR-USE-04). */
export function Results() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const report = store.report.value;
  const state = store.flow.value.jobState ?? report?.state ?? "done";

  if (!report) {
    return (
      <div class="screen">
        <ScreenTitle>{t("results-title", { state })}</ScreenTitle>
        {store.jobError.value ? <ErrorCallout error={store.jobError.value} /> : <Loading label={t("results-loading")} />}
      </div>
    );
  }

  const c = report.counts;
  const saved = c.done + c.metadata_partial + c.location_uncertain;

  return (
    <div class="screen">
      <ScreenTitle lede={t("results-lede", { count: saved })}>{t("results-title", { state })}</ScreenTitle>
      <p class="path">{report.output_path}</p>

      {store.jobError.value ? <ErrorCallout error={store.jobError.value} /> : null}

      {state === "cancelled" ? (
        <Callout tone="info" title={t("results-cancelled-title")} actions={<Button onClick={() => void store.resume()}><IconPlay size={18} />{t("results-continue-job")}</Button>}>
          <p>{t("results-cancelled-body")}</p>
        </Callout>
      ) : null}

      <Groups report={report} />
      <JoinedClips report={report} />

      <section class="card report-card" aria-labelledby="report-title">
        <h2 id="report-title" class="card-title">
          {t("results-report-title")}
        </h2>
        <p>{t("results-report-body")}</p>
        {report.report_html_path ? (
          <Button onClick={() => void store.openPath(report.report_html_path ?? "")}>
            <IconFile size={18} />
            {t("results-open-report")}
          </Button>
        ) : (
          <p class="muted">{t("results-report-pending")}</p>
        )}
      </section>

      {store.diagnosticsPath.value ? (
        <Callout tone="success" title={t("results-diagnostics-saved")} role="status">
          <p>{t("results-diagnostics-body")}</p>
          <p class="path">{store.diagnosticsPath.value}</p>
        </Callout>
      ) : null}

      <ActionBar>
        <Button variant="primary" onClick={() => void store.openOutput()}>
          <IconExternal size={18} />
          {t("results-open-folder")}
        </Button>
        <Button onClick={() => void store.previewDiagnostics()} busy={store.diagnosticsBusy.value}>
          {t("results-diagnostics")}
        </Button>
        <Button variant="ghost" onClick={() => void store.newJob()}>
          {t("results-new-job")}
        </Button>
      </ActionBar>
    </div>
  );
}

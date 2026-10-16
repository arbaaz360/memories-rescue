import { useI18n } from "../i18n/context";
import { errorMessageId } from "../i18n/l10n";
import { useStore } from "../state/context";
import { byteBaseFor, formatBytes, formatCalendarDate } from "../format/format";
import type { ExportSummary, Issue } from "../ipc/types";
import { ActionBar, Badge, Button, Callout, ErrorCallout, Loading, ScreenTitle, Stat } from "../ui/components";
import { IconArrowBack, IconCalendar, IconDisk, IconLayers, IconPhoto, IconPin, IconVideo, IconFile } from "../ui/icons";

/** Issue copy: `issue-<code>` when the count matters, else the §8 error copy. */
export function IssueCallout(props: { issue: Issue }) {
  const i18n = useI18n();
  const id = `issue-${props.issue.code.toLowerCase().replace(/_/g, "-")}`;
  const args = { count: props.issue.count ?? 0, part: props.issue.part_number ?? 0 };
  const known = i18n.has(id);
  const title = known ? i18n.t(id, args) : i18n.t(errorMessageId(props.issue.code), args);
  const body = known ? i18n.attr(id, "body", args) : i18n.attr(errorMessageId(props.issue.code), "next", args);
  const tone = props.issue.code === "PART_TRUNCATED" || props.issue.code === "JSON_PARSE" ? "danger" : "warning";
  return (
    <Callout tone={tone} title={title}>
      <p>{body}</p>
    </Callout>
  );
}

/** Fluent variant keys cannot start with a digit: "2026a" is selected as "v2026a". */
export function profileKey(profile: ExportSummary["profile"]): string {
  return /^\d/.test(profile) ? `v${profile}` : profile;
}

function ProfileCard(props: { summary: ExportSummary }) {
  const i18n = useI18n();
  const t = i18n.t;
  const { profile_counts: pc } = props.summary;
  const profile = profileKey(props.summary.profile);
  return (
    <section class="card" aria-labelledby="profile-title">
      <h2 id="profile-title" class="card-title">
        {t("summary-format-title")}
      </h2>
      <p class="profile-name">
        <Badge tone={profile === "legacy" || profile === "unknown" ? "warning" : "info"}>{t("summary-profile-name", { profile })}</Badge>
      </p>
      <p>{t("summary-profile-explain", { profile })}</p>
      {props.summary.profile !== "legacy" ? (
        <ul class="breakdown-list">
          {(
            [
              ["summary-path-exact", "summary-path-exact-desc", pc.exact],
              ["summary-path-ordinal", "summary-path-ordinal-desc", pc.ordinal],
              ["summary-path-day", "summary-path-day-desc", pc.day_only],
            ] as const
          ).map(([label, desc, count]) => (
            <li key={label}>
              <span class="breakdown-count">{i18n.num(count).toString()}</span>
              <span>
                <span class="breakdown-label">{t(label)}</span>
                <span class="field-desc block">{t(desc)}</span>
              </span>
            </li>
          ))}
        </ul>
      ) : null}
      {pc.ordinal > 0 ? <p class="field-desc">{t("summary-ordinal-note", { count: pc.ordinal })}</p> : null}
    </section>
  );
}

/** S2: format, part completeness with missing-part callouts, counts, date range, size, warnings. */
export function Summary() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const base = byteBaseFor(store.host.platform);
  const summary = store.summary.value;
  const busy = store.summaryBusy.value;
  const error = store.summaryError.value;
  const jobMode = store.flow.value.summaryMode === "add_part";

  const header = (
    <ScreenTitle lede={jobMode ? t("summary-lede-job") : t("summary-lede")}>{jobMode ? t("summary-title-job") : t("summary-title")}</ScreenTitle>
  );

  if (busy || (!summary && !error)) {
    return (
      <div class="screen">
        {header}
        <Loading label={t("summary-loading")} />
      </div>
    );
  }

  if (!summary) {
    return (
      <div class="screen">
        {header}
        {error ? <ErrorCallout error={error} /> : null}
        <ActionBar>
          <Button variant="ghost" onClick={() => store.back()}>
            <IconArrowBack size={18} />
            {t("action-back")}
          </Button>
        </ActionBar>
      </div>
    );
  }

  const missing = summary.parts.filter((p) => p.status === "missing");
  const otherIssues = summary.issues.filter((i) => i.code !== "PART_MISSING" && i.code !== "LEGACY_LINKS_ONLY");
  const c = summary.counts;

  return (
    <div class="screen">
      {header}
      {error ? <ErrorCallout error={error} /> : null}

      {summary.profile === "legacy" ? (
        <Callout tone="danger" title={t("summary-legacy-title")} role="alert">
          <p>{t("summary-legacy-body")}</p>
          <ol class="plain-steps">
            <li>{t("guide-step1-title")}</li>
            <li>{t("guide-step2-title")}</li>
            <li>{t("guide-step3-title")}</li>
            <li>{t("guide-step4-title")}</li>
          </ol>
        </Callout>
      ) : null}

      {missing.map((p) => {
        const issue = summary.issues.find((i) => i.code === "PART_MISSING" && i.part_number === p.part_number);
        return (
          <Callout
            key={p.part_number}
            tone="warning"
            title={t("summary-part-missing-title", { n: p.part_number })}
            role="status"
            actions={
              <>
                <Button variant="primary" onClick={() => void store.addMissingPart()} busy={busy}>
                  {t("summary-add-missing")}
                </Button>
                {!summary.blocked ? <Button onClick={() => void store.continueFromSummary()}>{t("summary-continue-anyway")}</Button> : null}
              </>
            }
          >
            <p>{t("summary-part-missing-body", { n: p.part_number, count: issue?.count ?? 0 })}</p>
          </Callout>
        );
      })}

      <div class="grid-2">
        <ProfileCard summary={summary} />
        <section class="card" aria-labelledby="parts-title">
          <h2 id="parts-title" class="card-title">
            {t("summary-parts-title", { count: summary.parts.length })}
          </h2>
          <ul class="parts-list">
            {summary.parts.map((p) => (
              <li key={p.part_number} class={`part part-${p.status}`}>
                <span class="part-name">{t("summary-part-label", { n: p.part_number })}</span>
                <span class="part-detail">
                  {p.status === "missing" ? (
                    <Badge tone="warning" icon>
                      {t("summary-part-status-missing")}
                    </Badge>
                  ) : p.status === "truncated" ? (
                    <Badge tone="danger" icon>
                      {t("summary-part-status-truncated")}
                    </Badge>
                  ) : (
                    <Badge tone="success" icon>
                      {t("summary-part-status-ok")}
                    </Badge>
                  )}
                  {p.status !== "missing" ? (
                    <span class="muted">{t("summary-part-meta", { size: formatBytes(p.size_bytes, i18n.formatLocale, base), count: p.entries })}</span>
                  ) : null}
                </span>
              </li>
            ))}
          </ul>
        </section>
      </div>

      <section aria-labelledby="counts-title">
        <h2 id="counts-title">{t("summary-counts-title")}</h2>
        <dl class="stats">
          <Stat icon={<IconFile size={18} />} label={t("summary-count-entries")} value={i18n.num(c.entries).toString()} />
          <Stat icon={<IconPhoto size={18} />} label={t("summary-count-photos")} value={i18n.num(c.photos).toString()} />
          <Stat icon={<IconVideo size={18} />} label={t("summary-count-videos")} value={i18n.num(c.videos).toString()} />
          <Stat
            icon={<IconLayers size={18} />}
            label={t("summary-count-overlays")}
            value={i18n.num(c.overlays).toString()}
            hint={c.orphan_overlays > 0 ? t("summary-count-orphans", { count: c.orphan_overlays }) : undefined}
          />
          <Stat icon={<IconPin size={18} />} label={t("summary-count-location")} value={i18n.num(c.with_location).toString()} />
          <Stat
            icon={<IconCalendar size={18} />}
            label={t("summary-count-dates")}
            value={
              summary.date_range.first && summary.date_range.last
                ? t("summary-date-range", {
                    first: formatCalendarDate(summary.date_range.first, i18n.formatLocale),
                    last: formatCalendarDate(summary.date_range.last, i18n.formatLocale),
                  })
                : t("summary-date-none")
            }
          />
          <Stat icon={<IconDisk size={18} />} label={t("summary-count-size")} value={formatBytes(summary.total_bytes, i18n.formatLocale, base)} />
        </dl>
      </section>

      {otherIssues.length > 0 ? (
        <section aria-labelledby="issues-title">
          <h2 id="issues-title">{t("summary-issues-title")}</h2>
          {otherIssues.map((issue, i) => (
            <IssueCallout key={`${issue.code}-${i}`} issue={issue} />
          ))}
        </section>
      ) : null}

      <ActionBar>
        <Button variant="ghost" onClick={() => store.back()}>
          <IconArrowBack size={18} />
          {jobMode ? t("summary-back-results") : t("action-back")}
        </Button>
        {missing.length > 0 ? <Button onClick={() => void store.addMissingPart()}>{t("summary-add-missing")}</Button> : null}
        <Button variant="primary" onClick={() => void store.continueFromSummary()} disabled={summary.blocked}>
          {jobMode ? t("summary-continue-job") : missing.length > 0 ? t("summary-continue-anyway") : t("action-continue")}
        </Button>
      </ActionBar>
    </div>
  );
}

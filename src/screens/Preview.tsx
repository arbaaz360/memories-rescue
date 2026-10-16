import { useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import { byteBaseFor, formatBytes, formatCaptureTime, formatCoordinate, formatDuration, formatNumber } from "../format/format";
import type { ConfidenceBreakdown, FolderNode, LocationBreakdown, PlanItem } from "../ipc/types";
import { ActionBar, Badge, Button, Callout, Checkbox, ErrorCallout, Loading, ScreenTitle, Stat } from "../ui/components";
import { IconArrowBack, IconFolder, IconPin, IconPinOff, IconPhoto, IconVideo, IconExternal } from "../ui/icons";
import { IssueCallout } from "./Summary";
import { HevcNotice } from "../ui/HevcNotice";

function Breakdown(props: {
  title: string;
  parts: { key: string; count: number; label: string; tone: "a" | "b" | "c" }[];
}) {
  const i18n = useI18n();
  const total = props.parts.reduce((n, p) => n + p.count, 0) || 1;
  return (
    <div class="breakdown">
      <h3 class="breakdown-title">{props.title}</h3>
      <div class="stack-bar" aria-hidden="true">
        {props.parts.map((p) =>
          p.count > 0 ? <span key={p.key} class={`stack-seg seg-${p.tone}`} style={{ inlineSize: `${(p.count / total) * 100}%` }} /> : null,
        )}
      </div>
      <ul class="legend">
        {props.parts.map((p) => (
          <li key={p.key}>
            <span class={`swatch seg-${p.tone}`} aria-hidden="true" />
            <span class="legend-count">{formatNumber(p.count, i18n.formatLocale)}</span>
            <span>{p.label}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function ConfidenceView(props: { confidence: ConfidenceBreakdown; location: LocationBreakdown }) {
  const t = useI18n().t;
  const { confidence: c, location: l } = props;
  return (
    <div class="grid-2">
      <Breakdown
        title={t("preview-confidence-title")}
        parts={[
          { key: "exact", count: c.exact, label: t("confidence-exact", { count: c.exact }), tone: "a" },
          { key: "ordinal", count: c.ordinal, label: t("confidence-ordinal", { count: c.ordinal }), tone: "b" },
          { key: "none", count: c.none, label: t("confidence-none", { count: c.none }), tone: "c" },
        ]}
      />
      <Breakdown
        title={t("preview-location-title")}
        parts={[
          { key: "applied", count: l.applied, label: t("location-applied", { count: l.applied }), tone: "a" },
          { key: "uncertain", count: l.uncertain, label: t("location-uncertain", { count: l.uncertain }), tone: "c" },
          { key: "none", count: l.none, label: t("location-none", { count: l.none }), tone: "b" },
        ]}
      />
    </div>
  );
}

function Tree(props: { node: FolderNode; depth: number }) {
  const i18n = useI18n();
  const t = i18n.t;
  const { node, depth } = props;
  const files = node.file_count > 0 ? <span class="tree-count">{t("preview-tree-files", { count: node.file_count })}</span> : null;
  const samples =
    node.sample_files.length > 0 ? (
      <ul class="tree-files">
        {node.sample_files.map((f) => (
          <li key={f}>{f}</li>
        ))}
        {node.file_count > node.sample_files.length ? <li class="muted">{t("preview-tree-more", { count: node.file_count - node.sample_files.length })}</li> : null}
      </ul>
    ) : null;
  if (node.children.length === 0) {
    return (
      <li class="tree-leaf">
        <span class="tree-name">
          <IconFolder size={16} />
          {node.name}
        </span>
        {files}
        {samples}
      </li>
    );
  }
  const total = (n: FolderNode): number => n.file_count + n.children.reduce((s, ch) => s + total(ch), 0);
  return (
    <li>
      <details open={depth === 0}>
        <summary>
          <span class="tree-name">
            <IconFolder size={16} />
            {node.name}
          </span>
          <span class="tree-count">{t("preview-tree-files", { count: total(node) })}</span>
        </summary>
        {samples}
        <ul class="tree">
          {node.children.map((ch) => (
            <Tree key={ch.name} node={ch} depth={depth + 1} />
          ))}
        </ul>
      </details>
    </li>
  );
}

function Coordinates(props: { item: PlanItem }) {
  const i18n = useI18n();
  const t = i18n.t;
  const { latitude: lat, longitude: lon, location_decision: d } = props.item;
  // The engine sends no coordinates for an uncertain match it does not apply.
  if (d === "uncertain" && (lat === null || lon === null)) {
    return (
      <span class="loc loc-uncertain">
        <IconPinOff size={16} />
        {t("preview-loc-uncertain")}
      </span>
    );
  }
  if (lat === null || lon === null || d === "none") {
    return (
      <span class="loc loc-none">
        <IconPinOff size={16} />
        {t("preview-loc-none")}
      </span>
    );
  }
  const text = t("preview-coords", {
    lat: formatCoordinate(lat, i18n.formatLocale),
    ns: lat >= 0 ? "n" : "s",
    lon: formatCoordinate(lon, i18n.formatLocale),
    ew: lon >= 0 ? "e" : "w",
  });
  if (d === "uncertain") {
    return (
      <span class="loc loc-uncertain">
        <IconPinOff size={16} />
        <span>
          {t("preview-loc-uncertain")}
          <span class="loc-coords">{text}</span>
        </span>
      </span>
    );
  }
  return (
    <span class="loc">
      <IconPin size={16} />
      <span class="loc-coords">{text}</span>
    </span>
  );
}

/** S4: dry-run preview. Nothing is written except the optional preview report. */
export function Preview() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const base = byteBaseFor(store.host.platform);
  const plan = store.plan.value;
  const options = store.options.value;
  const header = <ScreenTitle lede={t("preview-lede")}>{t("preview-title")}</ScreenTitle>;

  if (store.planBusy.value || (!plan && !store.planError.value)) {
    return (
      <div class="screen">
        {header}
        <Loading label={t("preview-loading")} />
      </div>
    );
  }
  if (!plan || !options) {
    return (
      <div class="screen">
        {header}
        {store.planError.value ? <ErrorCallout error={store.planError.value} /> : null}
        <ActionBar>
          <Button variant="ghost" onClick={() => store.back()}>
            <IconArrowBack size={18} />
            {t("action-back")}
          </Button>
        </ActionBar>
      </div>
    );
  }

  const excluded = new Set(options.excluded_clip_groups);
  const duration = formatDuration(plan.estimated_seconds, i18n.formatLocale);
  const tot = plan.totals;

  return (
    <div class="screen">
      {header}
      {store.planError.value ? <ErrorCallout error={store.planError.value} /> : null}

      <section aria-labelledby="totals-title">
        <h2 id="totals-title">{t("preview-totals-title")}</h2>
        <dl class="stats">
          <Stat label={t("preview-total-items")} value={formatNumber(tot.items, i18n.formatLocale)} />
          <Stat label={t("preview-total-photos")} value={formatNumber(tot.photos, i18n.formatLocale)} />
          <Stat label={t("preview-total-videos")} value={formatNumber(tot.videos, i18n.formatLocale)} />
          <Stat label={t("preview-total-captions")} value={formatNumber(tot.overlays_to_composite, i18n.formatLocale)} />
          <Stat label={t("preview-total-originals")} value={formatNumber(tot.originals_kept, i18n.formatLocale)} />
          <Stat label={t("preview-total-missing")} value={formatNumber(tot.missing_media, i18n.formatLocale)} hint={tot.missing_media > 0 ? t("preview-total-missing-hint") : undefined} />
          <Stat label={t("preview-estimate-time")} value={duration ? t("preview-estimate-duration", { duration }) : t("preview-estimate-soon")} />
          <Stat
            label={t("preview-estimate-size")}
            value={formatBytes(plan.space.total_bytes, i18n.formatLocale, base)}
            hint={t("preview-estimate-free", { free: formatBytes(plan.space.free_bytes, i18n.formatLocale, base), volume: plan.space.volume })}
          />
        </dl>
      </section>

      <HevcNotice
        notice={plan.hevc_notice}
        onEnable={() => {
          store.setOptions({ windows_compatibility: true });
          void store.preview();
        }}
      />

      {plan.free_limit.applies && plan.free_limit.items_beyond > 0 ? (
        <Callout
          tone="info"
          title={t("preview-free-title", { limit: plan.free_limit.limit })}
          actions={<Button onClick={() => store.openUnlock("header")}>{t("preview-free-unlock")}</Button>}
        >
          <p>{t("preview-free-body", { limit: plan.free_limit.limit, free: plan.free_limit.free_items, beyond: plan.free_limit.items_beyond, total: tot.items })}</p>
        </Callout>
      ) : null}

      <section aria-labelledby="confidence-title">
        <h2 id="confidence-title">{t("preview-match-title")}</h2>
        <ConfidenceView confidence={plan.confidence} location={plan.location} />
      </section>

      <section aria-labelledby="samples-title">
        <h2 id="samples-title">{t("preview-samples-title")}</h2>
        <div class="table-wrap" role="region" aria-labelledby="samples-title" tabIndex={0}>
          <table class="samples">
            <thead>
              <tr>
                <th scope="col">{t("preview-col-preview")}</th>
                <th scope="col">{t("preview-col-name")}</th>
                <th scope="col">{t("preview-col-date")}</th>
                <th scope="col">{t("preview-col-location")}</th>
              </tr>
            </thead>
            <tbody>
              {plan.samples.map((s) => {
                const name = s.planned_path.split("/").pop() ?? s.planned_path;
                return (
                  <tr key={s.item_id}>
                    <td>
                      <span class="thumb">
                        {s.thumbnail ? <img src={s.thumbnail} alt="" width={64} height={53} /> : null}
                        <span class="thumb-kind">
                          {s.media_type === "video" ? <IconVideo size={14} /> : <IconPhoto size={14} />}
                          <span class="visually-hidden">{t("preview-kind", { kind: s.media_type })}</span>
                        </span>
                      </span>
                    </td>
                    <td>
                      <span class="mono">{name}</span>
                      <span class="cell-sub">
                        {t("preview-folder", { folder: s.planned_path.split("/").slice(0, -1).join("/") })}
                        {s.has_overlay ? <Badge>{t("preview-has-caption")}</Badge> : null}
                      </span>
                    </td>
                    <td>
                      {s.capture_time ? formatCaptureTime(s.capture_time, i18n.formatLocale, s.time_precision) : t("preview-no-date")}
                      <span class="cell-sub">
                        <Badge tone={s.match_confidence === "exact" ? "success" : s.match_confidence === "ordinal" ? "info" : "warning"}>
                          {t("preview-match-badge", { confidence: s.match_confidence })}
                        </Badge>
                      </span>
                    </td>
                    <td>
                      <Coordinates item={s} />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </section>

      <div class="grid-2">
        <section class="card" aria-labelledby="tree-title">
          <h2 id="tree-title" class="card-title">
            {t("preview-tree-title")}
          </h2>
          <ul class="tree tree-root">
            <Tree node={plan.tree} depth={0} />
          </ul>
        </section>

        <section class="card" aria-labelledby="clips-title">
          <h2 id="clips-title" class="card-title">
            {t("preview-clips-title", { count: plan.clip_groups.length })}
          </h2>
          {plan.clip_groups.length === 0 ? (
            <p class="muted">{options.clip_rejoin ? t("preview-clips-none") : t("preview-clips-off")}</p>
          ) : (
            <>
              <p class="field-desc">{t("preview-clips-intro")}</p>
              {plan.clip_groups.map((g) => (
                <Checkbox
                  key={g.group_id}
                  checked={!excluded.has(g.group_id)}
                  onChange={(v) => store.toggleClipGroup(g.group_id, v)}
                  label={t("preview-clip-label", {
                    count: g.item_ids.length,
                    seconds: Math.round(g.total_seconds),
                    date: formatCaptureTime(g.start_time, i18n.formatLocale),
                  })}
                  description={<span class="mono">{g.planned_path}</span>}
                />
              ))}
            </>
          )}
        </section>
      </div>

      {plan.warnings.length > 0 ? (
        <section aria-labelledby="pwarn-title">
          <h2 id="pwarn-title">{t("preview-warnings-title")}</h2>
          {plan.warnings.map((w, i) => (
            <IssueCallout key={`${w.code}-${i}`} issue={w} />
          ))}
        </section>
      ) : null}

      {store.previewReportPath.value ? (
        <Callout
          tone="success"
          title={t("preview-report-saved")}
          role="status"
          actions={
            <Button onClick={() => void store.openPath(store.previewReportPath.value ?? "")}>
              <IconExternal size={18} />
              {t("preview-report-open")}
            </Button>
          }
        >
          <p class="path">{store.previewReportPath.value}</p>
        </Callout>
      ) : null}

      <ActionBar>
        <Button variant="ghost" onClick={() => store.back()}>
          <IconArrowBack size={18} />
          {t("action-back")}
        </Button>
        <Button onClick={() => void store.exportPreviewReport()}>{t("preview-report-export")}</Button>
        <Button variant="primary" onClick={() => void store.start()} disabled={!store.canStart.value} busy={store.startBusy.value}>
          {t("action-start")}
        </Button>
      </ActionBar>
    </div>
  );
}

import { useState } from "preact/hooks";
import { useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import { byteBaseFor, durationParts, formatBytes, formatDuration, formatPercent, roundRate } from "../format/format";
import type { ItemStatus } from "../ipc/types";
import { ActionBar, Badge, Button, Callout, Dialog, ErrorCallout, ProgressBar, ScreenTitle, Stat } from "../ui/components";
import { IconCoffee, IconExternal, IconPause, IconPhoto, IconPlay, IconStop, IconVideo } from "../ui/icons";

export function statusTone(status: ItemStatus): "success" | "warning" | "danger" | "info" | "neutral" {
  switch (status) {
    case "done":
      return "success";
    case "metadata_partial":
    case "location_uncertain":
      return "warning";
    case "failed":
      return "danger";
    case "missing_media":
      return "neutral";
    case "skipped_free_limit":
      return "info";
  }
}

/** S5: progress, ETA, throughput, stage, recent items, Pause / Resume / Cancel / Open output folder. */
export function Processing() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const base = byteBaseFor(store.host.platform);
  const p = store.progress.value;
  const state = store.flow.value.jobState;
  const [confirmCancel, setConfirmCancel] = useState(false);
  const license = store.license.value;

  const running = state === "running";
  const paused = state === "paused";
  const title =
    state === "paused" ? t("processing-title-paused") : state === "free_limit_reached" ? t("processing-title-free-limit") : t("processing-title");

  const eta = p?.eta_seconds ?? null;
  const etaText =
    !running || eta === null
      ? running
        ? t("processing-eta-unknown")
        : t("processing-eta-paused")
      : durationParts(eta).kind === "under_minute"
        ? t("processing-eta-soon")
        : t("processing-eta", { duration: formatDuration(eta, i18n.formatLocale) ?? "" });
  const rate = roundRate(p?.throughput_items_per_s ?? 0);

  return (
    <div class="screen">
      <ScreenTitle lede={t("processing-lede")}>{title}</ScreenTitle>

      <section class="card progress-card" aria-labelledby="overall-title">
        <h2 id="overall-title" class="visually-hidden">
          {t("processing-overall")}
        </h2>
        <div class="progress-head">
          <span class="progress-pct" aria-hidden="true">
            {formatPercent(p?.overall_pct ?? 0, i18n.formatLocale)}
          </span>
          <span class="progress-stage">{t("processing-stage", { stage: p?.stage ?? "indexing" })}</span>
        </div>
        <ProgressBar
          pct={p?.overall_pct ?? 0}
          label={t("processing-overall")}
          indeterminate={!p || p.stage === "indexing" || p.stage === "matching"}
          valueText={p ? t("processing-valuetext", { pct: formatPercent(p.overall_pct, i18n.formatLocale), done: p.items_done, total: p.items_total }) : undefined}
        />
        <dl class="stats stats-compact">
          <Stat label={t("processing-items")} value={p ? t("processing-of", { done: p.items_done, total: p.items_total }) : "–"} />
          <Stat
            label={t("processing-data")}
            value={
              p
                ? t("processing-of-text", {
                    done: formatBytes(p.bytes_done, i18n.formatLocale, base),
                    total: formatBytes(p.bytes_total, i18n.formatLocale, base),
                  })
                : "–"
            }
          />
          <Stat label={t("processing-time-left")} value={etaText} />
          <Stat label={t("processing-speed")} value={t("processing-rate", { rate: i18n.num(rate.value, { maximumFractionDigits: rate.digits, minimumFractionDigits: rate.digits }) })} />
        </dl>
        <p class="chips">
          <Badge tone={p && p.warnings > 0 ? "warning" : "neutral"}>{t("processing-warnings", { count: p?.warnings ?? 0 })}</Badge>
          <Badge tone={p && p.failures > 0 ? "danger" : "neutral"}>{t("processing-failures", { count: p?.failures ?? 0 })}</Badge>
        </p>
        {p?.sleep_prevented ? (
          <p class="sleep-note">
            <IconCoffee size={18} />
            {t("processing-awake")}
          </p>
        ) : null}
        {license?.tier === "free" && state !== "free_limit_reached" ? <p class="free-note">{t("processing-free-note", { limit: license.free_limit })}</p> : null}
      </section>

      {store.jobError.value ? (
        <ErrorCallout
          error={store.jobError.value}
          tone="warning"
          actions={
            paused ? (
              <Button variant="primary" onClick={() => void store.resume()}>
                {t("processing-resume")}
              </Button>
            ) : undefined
          }
        />
      ) : null}

      {state === "free_limit_reached" && !store.flow.value.unlock ? (
        <Callout
          tone="info"
          title={t("processing-free-limit-title", { limit: license?.free_limit ?? 100 })}
          actions={
            <>
              <Button variant="primary" onClick={() => store.openUnlock("free_limit")}>
                {t("processing-unlock")}
              </Button>
              <Button onClick={() => void store.later()}>{t("processing-finish-free")}</Button>
            </>
          }
        >
          <p>{t("processing-free-limit-body")}</p>
        </Callout>
      ) : null}

      <section aria-labelledby="recent-title">
        <h2 id="recent-title">{t("processing-recent-title")}</h2>
        <div class="recent-wrap" role="region" aria-labelledby="recent-title" tabIndex={0}>
          {p && p.recent_items.length > 0 ? (
            <ul class="recent-list">
              {p.recent_items.map((it) => (
                <li key={`${it.item_id}-${it.finished_at}`} class="recent-item">
                  <span class="recent-kind">
                    {it.media_type === "video" ? <IconVideo size={16} /> : <IconPhoto size={16} />}
                    <span class="visually-hidden">{t("preview-kind", { kind: it.media_type })}</span>
                  </span>
                  <span class="mono recent-name">{it.name}</span>
                  <Badge tone={statusTone(it.status)}>{t("status-short", { status: it.status })}</Badge>
                </li>
              ))}
            </ul>
          ) : (
            <p class="muted">{t("processing-recent-empty")}</p>
          )}
        </div>
      </section>

      <ActionBar>
        {paused ? (
          <Button variant="primary" onClick={() => void store.resume()} busy={store.jobBusy.value}>
            <IconPlay size={18} />
            {t("processing-resume")}
          </Button>
        ) : (
          <Button onClick={() => void store.pause()} disabled={!running} busy={store.jobBusy.value}>
            <IconPause size={18} />
            {t("processing-pause")}
          </Button>
        )}
        <Button variant="danger" onClick={() => setConfirmCancel(true)} disabled={!running && !paused && state !== "free_limit_reached"}>
          <IconStop size={18} />
          {t("processing-cancel")}
        </Button>
        <Button onClick={() => void store.openOutput()}>
          <IconExternal size={18} />
          {t("processing-open-output")}
        </Button>
      </ActionBar>

      <Dialog
        open={confirmCancel}
        title={t("cancel-title")}
        onClose={() => setConfirmCancel(false)}
        footer={
          <>
            <Button onClick={() => setConfirmCancel(false)}>{t("cancel-keep-going")}</Button>
            <Button
              variant="danger"
              onClick={() => {
                setConfirmCancel(false);
                void store.cancel();
              }}
            >
              {t("cancel-confirm")}
            </Button>
          </>
        }
      >
        <p>{t("cancel-body")}</p>
      </Dialog>
    </div>
  );
}

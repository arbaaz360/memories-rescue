import { useId, useMemo } from "preact/hooks";
import { useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import { byteBaseFor, formatBytes, joinPath } from "../format/format";
import type { JobOptions, OverlayMode } from "../ipc/types";
import { ActionBar, Button, Callout, Checkbox, ErrorCallout, RadioGroup, ScreenTitle, SpaceMeter } from "../ui/components";
import { IconArrowBack, IconFolder } from "../ui/icons";
import { HevcNotice } from "../ui/HevcNotice";

const OVERLAY_MODES: readonly OverlayMode[] = ["burn_in_keep_originals", "burn_in_only", "separate_file", "ignore"];
const SPACE_MARGIN = 1.1;

export function timeZones(current: string): string[] {
  let zones: string[] = [];
  try {
    zones = Intl.supportedValuesOf("timeZone");
  } catch {
    zones = ["UTC", "Europe/London", "America/New_York", "Asia/Kolkata", "Australia/Sydney"];
  }
  if (current && !zones.includes(current)) zones = [current, ...zones];
  return zones;
}

export function burnsIn(o: Pick<JobOptions, "overlay_mode" | "metadata_only">): boolean {
  return !o.metadata_only && (o.overlay_mode === "burn_in_keep_originals" || o.overlay_mode === "burn_in_only");
}

/** Year range inputs for clip rejoin; "all years" is `null`. */
export function YearRange(props: { value: JobOptions["clip_rejoin_years"]; onChange: (v: JobOptions["clip_rejoin_years"]) => void; disabled: boolean }) {
  const t = useI18n().t;
  const id = useId();
  const all = props.value === null;
  const range = props.value ?? { from: 2017, to: 2021 };
  const year = new Date().getFullYear();
  return (
    <fieldset class="year-range" disabled={props.disabled}>
      <legend class="visually-hidden">{t("options-rejoin-years-legend")}</legend>
      <div class="inline-fields">
        <label for={`${id}-from`}>{t("options-rejoin-from")}</label>
        <input
          id={`${id}-from`}
          type="number"
          inputMode="numeric"
          min={2011}
          max={year}
          value={range.from}
          disabled={all || props.disabled}
          onChange={(e) => {
            const from = Number((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(from)) props.onChange({ from, to: Math.max(from, range.to) });
          }}
        />
        <label for={`${id}-to`}>{t("options-rejoin-to")}</label>
        <input
          id={`${id}-to`}
          type="number"
          inputMode="numeric"
          min={2011}
          max={year}
          value={range.to}
          disabled={all || props.disabled}
          onChange={(e) => {
            const to = Number((e.currentTarget as HTMLInputElement).value);
            if (Number.isFinite(to)) props.onChange({ from: Math.min(range.from, to), to });
          }}
        />
      </div>
      <Checkbox
        checked={all}
        disabled={props.disabled}
        onChange={(checked) => props.onChange(checked ? null : { from: 2017, to: 2021 })}
        label={t("options-rejoin-all-years")}
      />
    </fieldset>
  );
}

/** S3: destination with free-space gauge and live estimate, and the job options of FR-26 / FR-48. */
export function Options() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const base = byteBaseFor(store.host.platform);
  const o = store.options.value;
  const space = store.space.value;
  const volume = store.volume.value;
  const license = store.license.value;
  const tzId = useId();
  const zipId = useId();
  const meterDesc = useId();
  const zones = useMemo(() => timeZones(o?.home_timezone ?? ""), [o?.home_timezone]);

  if (!o) return null;
  const set = (patch: Partial<JobOptions>) => store.setOptions(patch);
  const bytes = (n: number) => formatBytes(n, i18n.formatLocale, base);
  const burnIn = burnsIn(o);

  const usedPct = volume && volume.total_bytes > 0 ? ((volume.total_bytes - volume.free_bytes) / volume.total_bytes) * 100 : 0;
  // FR-28: the job may start when free space is at least the estimate plus 10%.
  const needWithMargin = space ? space.total_bytes * SPACE_MARGIN : 0;
  const needPct = volume && space && volume.total_bytes > 0 ? (needWithMargin / volume.total_bytes) * 100 : 0;

  return (
    <div class="screen">
      <ScreenTitle lede={t("options-lede")}>{t("options-title")}</ScreenTitle>
      <HevcNotice notice={store.summary.value?.hevc_notice ?? null} onEnable={() => set({ windows_compatibility: true })} />

      <section class="card" aria-labelledby="dest-title">
        <h2 id="dest-title" class="card-title">
          {t("options-dest-title")}
        </h2>
        <div class="dest-row">
          <p class="path" aria-label={t("options-dest-current")}>
            {o.destination ? joinPath(o.destination, "Memories Rescue") : t("options-dest-none")}
          </p>
          <Button onClick={() => void store.chooseDestination()}>
            <IconFolder size={18} />
            {t("options-dest-choose")}
          </Button>
        </div>
        <p class="field-desc">{t("options-dest-hint")}</p>
        {volume && space ? (
          <>
            <SpaceMeter
              label={t("options-space-label", { volume: volume.volume })}
              usedPct={usedPct}
              needPct={needPct}
              sufficient={space.sufficient}
              valueText={t("options-space-text", { need: bytes(needWithMargin), free: bytes(space.free_bytes), volume: space.volume })}
              describedBy={meterDesc}
            />
            <p id={meterDesc} class="meter-legend">
              <span class="legend-item">
                <span class="swatch swatch-used" aria-hidden="true" />
                {t("options-space-used")}
              </span>
              <span class="legend-item">
                <span class="swatch swatch-need" aria-hidden="true" />
                {t("options-space-need")}
              </span>
              <span>{t("options-space-text", { need: bytes(needWithMargin), free: bytes(space.free_bytes), volume: space.volume })}</span>
            </p>
            <details class="space-details">
              <summary>{t("options-space-breakdown")}</summary>
              <dl class="kv">
                <dt>{t("options-space-output")}</dt>
                <dd>{bytes(space.output_bytes)}</dd>
                <dt>{t("options-space-originals")}</dt>
                <dd>{bytes(space.originals_bytes)}</dd>
                <dt>{t("options-space-temp")}</dt>
                <dd>{bytes(space.temp_bytes)}</dd>
                <dt>{t("options-space-zip")}</dt>
                <dd>{bytes(space.zip_bytes)}</dd>
                <dt>{t("options-space-margin")}</dt>
                <dd>{bytes(needWithMargin - space.total_bytes)}</dd>
              </dl>
            </details>
            {!space.sufficient ? (
              <Callout
                tone="danger"
                role="alert"
                title={t("options-space-short-title", { volume: space.volume, short: bytes(space.shortfall_bytes) })}
                actions={
                  <>
                    <Button onClick={() => void store.chooseDestination()}>{t("options-space-fix-dest")}</Button>
                    {burnIn && o.keep_originals ? (
                      <Button onClick={() => set({ keep_originals: false, overlay_mode: "burn_in_only" })}>{t("options-space-fix-originals")}</Button>
                    ) : null}
                    {o.zip_output ? <Button onClick={() => set({ zip_output: false })}>{t("options-space-fix-zip")}</Button> : null}
                  </>
                }
              >
                <p>{t("options-space-short-body")}</p>
              </Callout>
            ) : null}
          </>
        ) : null}
      </section>

      <section class="card" aria-labelledby="overlay-title">
        <h2 id="overlay-title" class="card-title">
          {t("options-overlay-title")}
        </h2>
        <RadioGroup<OverlayMode>
          name="overlay_mode"
          legend={<span class="visually-hidden">{t("options-overlay-title")}</span>}
          value={o.overlay_mode}
          onChange={(mode) =>
            set({
              overlay_mode: mode,
              keep_originals: mode === "burn_in_keep_originals" ? true : mode === "burn_in_only" ? false : o.keep_originals,
            })
          }
          options={OVERLAY_MODES.map((m) => ({ value: m, label: t(`overlay-${m.replace(/_/g, "-")}`), description: t(`overlay-${m.replace(/_/g, "-")}-desc`) }))}
        />
        <Checkbox
          checked={burnIn && o.keep_originals}
          disabled={!burnIn}
          onChange={(keep) => set({ keep_originals: keep, overlay_mode: keep ? "burn_in_keep_originals" : "burn_in_only" })}
          label={t("options-keep-originals")}
          description={burnIn ? t("options-keep-originals-desc") : t("options-keep-originals-na")}
        />
      </section>

      <section class="card" aria-labelledby="more-title">
        <h2 id="more-title" class="card-title">
          {t("options-more-title")}
        </h2>
        <Checkbox
          checked={o.clip_rejoin}
          disabled={o.metadata_only}
          onChange={(v) => set({ clip_rejoin: v })}
          label={t("options-rejoin")}
          description={t("options-rejoin-desc")}
        />
        {o.clip_rejoin && !o.metadata_only ? (
          <div class="nested">
            <YearRange value={o.clip_rejoin_years} disabled={false} onChange={(v) => set({ clip_rejoin_years: v })} />
          </div>
        ) : null}
        <Checkbox
          checked={o.apply_uncertain_locations}
          onChange={(v) => set({ apply_uncertain_locations: v })}
          label={t("options-uncertain")}
          description={t("options-uncertain-desc")}
        />
        <Checkbox
          checked={o.windows_compatibility}
          onChange={(v) => set({ windows_compatibility: v })}
          label={t("options-wincompat")}
          description={t("options-wincompat-desc")}
        />
        <Checkbox checked={o.zip_output} onChange={(v) => set({ zip_output: v })} label={t("options-zip")} description={t("options-zip-desc")} />
        {o.zip_output ? (
          <div class="nested field">
            <label for={zipId}>{t("options-zip-volume")}</label>
            <select
              id={zipId}
              value={o.zip_volume_bytes === null ? "none" : "fat32"}
              onChange={(e) => set({ zip_volume_bytes: (e.currentTarget as HTMLSelectElement).value === "none" ? null : 4_000_000_000 })}
            >
              <option value="none">{t("options-zip-volume-none")}</option>
              <option value="fat32">{t("options-zip-volume-fat32")}</option>
            </select>
          </div>
        ) : null}
        <Checkbox
          checked={o.metadata_only}
          onChange={(v) => set({ metadata_only: v })}
          label={t("options-metadata-only")}
          description={t("options-metadata-only-desc")}
        />
        <div class="field">
          <label for={tzId}>{t("options-timezone")}</label>
          <select id={tzId} value={o.home_timezone} aria-describedby={`${tzId}-desc`} onChange={(e) => set({ home_timezone: (e.currentTarget as HTMLSelectElement).value })}>
            {zones.map((z) => (
              <option key={z} value={z}>
                {z.replace(/_/g, " ")}
              </option>
            ))}
          </select>
          <p id={`${tzId}-desc`} class="field-desc">
            {t("options-timezone-desc")}
          </p>
        </div>
      </section>

      {store.optionsError.value ? <ErrorCallout error={store.optionsError.value} /> : null}

      {license?.tier === "free" ? (
        <p class="free-note">{t("options-free-note", { limit: license.free_limit })}</p>
      ) : null}

      <ActionBar>
        <Button variant="ghost" onClick={() => store.back()}>
          <IconArrowBack size={18} />
          {t("action-back")}
        </Button>
        <Button onClick={() => void store.preview()} disabled={!o.destination}>
          {t("options-preview")}
        </Button>
        <Button variant="primary" onClick={() => void store.start()} disabled={!store.canStart.value} busy={store.startBusy.value}>
          {t("action-start")}
        </Button>
      </ActionBar>
    </div>
  );
}

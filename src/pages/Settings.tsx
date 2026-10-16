import { useEffect, useId, useMemo, useState } from "preact/hooks";
import { useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import { formatInstant } from "../format/format";
import type { OverlayMode, Settings, TelemetryPayload } from "../ipc/types";
import { ActionBar, Badge, Button, Callout, Checkbox, Dialog, RadioGroup, ScreenTitle } from "../ui/components";
import { IconArrowBack, IconExternal } from "../ui/icons";
import { YearRange, timeZones } from "../screens/Options";

/** mr-job's output template tokens (docs/api/mr-job.md, SRS 3.7). */
const TEMPLATE_TOKENS = ["{date}", "{time}", "{shortid}", "{id}", "{ext}"] as const;

/** Output template check: known tokens only, an id and the extension, no characters Windows refuses. */
export function templateProblem(template: string): "empty" | "token" | "chars" | "id" | "ext" | null {
  if (template.trim().length === 0) return "empty";
  const unknown = template.replace(/\{[^}]*\}/g, (tok) => (TEMPLATE_TOKENS.includes(tok as (typeof TEMPLATE_TOKENS)[number]) ? "" : "\u0000"));
  if (unknown.includes("\u0000") || /[{}]/.test(unknown)) return "token";
  if (/[<>:"/\\|?*]/.test(unknown)) return "chars";
  if (!template.includes("{shortid}") && !template.includes("{id}")) return "id";
  if (!template.includes("{ext}")) return "ext";
  return null;
}

export function templateExample(template: string): string {
  return template
    .replace("{date}", "2019-07-04")
    .replace("{time}", "210355")
    .replace("{shortid}", "3F2A9C1E")
    .replace("{id}", "3F2A9C1E-0B4D-4E8A-9C21-6D7E5F104A22")
    .replace("{ext}", "jpg");
}

const OVERLAY_MODES: readonly OverlayMode[] = ["burn_in_keep_originals", "burn_in_only", "separate_file", "ignore"];

/** Settings (FR-48, FR-47 licence management, NFR-PRV-04 telemetry preview, §10.2 update channel). */
export function SettingsPage() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const saved = store.settings.value;
  const license = store.license.value;
  const [draft, setDraft] = useState<Settings | null>(saved);
  const [payload, setPayload] = useState<TelemetryPayload | null>(null);
  const [confirmDeactivate, setConfirmDeactivate] = useState(false);
  const [saving, setSaving] = useState(false);
  const ids = { tz: useId(), workers: useId(), template: useId(), channel: useId(), language: useId(), zip: useId() };
  const zones = useMemo(() => timeZones(draft?.home_timezone ?? ""), [draft?.home_timezone]);

  useEffect(() => setDraft(saved), [saved]);
  useEffect(() => {
    void store.telemetryPreview().then(setPayload).catch(() => setPayload(null));
  }, [store]);

  if (!draft || !saved) return null;
  const set = (patch: Partial<Settings>) => setDraft({ ...draft, ...patch });
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved);
  const problem = templateProblem(draft.output_template);
  const burnIn = draft.overlay_mode === "burn_in_keep_originals" || draft.overlay_mode === "burn_in_only";
  const cores = typeof navigator !== "undefined" ? navigator.hardwareConcurrency || 4 : 4;

  const save = async () => {
    if (problem) return;
    setSaving(true);
    try {
      await store.saveSettings(draft);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div class="screen page">
      <ScreenTitle lede={t("settings-lede")}>{t("settings-title")}</ScreenTitle>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <section class="card" aria-labelledby="s-processing">
          <h2 id="s-processing" class="card-title">
            {t("settings-processing")}
          </h2>
          <RadioGroup<OverlayMode>
            name="settings_overlay_mode"
            legend={t("options-overlay-title")}
            value={draft.overlay_mode}
            onChange={(mode) =>
              set({ overlay_mode: mode, keep_originals: mode === "burn_in_keep_originals" ? true : mode === "burn_in_only" ? false : draft.keep_originals })
            }
            options={OVERLAY_MODES.map((m) => ({ value: m, label: t(`overlay-${m.replace(/_/g, "-")}`) }))}
          />
          <Checkbox
            checked={burnIn && draft.keep_originals}
            disabled={!burnIn}
            onChange={(keep) => set({ keep_originals: keep, overlay_mode: keep ? "burn_in_keep_originals" : "burn_in_only" })}
            label={t("options-keep-originals")}
            description={burnIn ? t("options-keep-originals-desc") : t("options-keep-originals-na")}
          />
          <Checkbox checked={draft.clip_rejoin} onChange={(v) => set({ clip_rejoin: v })} label={t("options-rejoin")} description={t("options-rejoin-desc")} />
          {draft.clip_rejoin ? (
            <div class="nested">
              <YearRange value={draft.clip_rejoin_years} disabled={false} onChange={(v) => set({ clip_rejoin_years: v })} />
            </div>
          ) : null}
          <Checkbox
            checked={draft.apply_uncertain_locations}
            onChange={(v) => set({ apply_uncertain_locations: v })}
            label={t("options-uncertain")}
            description={t("options-uncertain-desc")}
          />
          <Checkbox checked={draft.windows_compatibility} onChange={(v) => set({ windows_compatibility: v })} label={t("options-wincompat")} description={t("options-wincompat-desc")} />
          <Checkbox checked={draft.zip_output} onChange={(v) => set({ zip_output: v })} label={t("options-zip")} description={t("options-zip-desc")} />
          {draft.zip_output ? (
            <div class="nested field">
              <label for={ids.zip}>{t("options-zip-volume")}</label>
              <select
                id={ids.zip}
                value={draft.zip_volume_bytes === null ? "none" : "fat32"}
                onChange={(e) => set({ zip_volume_bytes: (e.currentTarget as HTMLSelectElement).value === "none" ? null : 4_000_000_000 })}
              >
                <option value="none">{t("options-zip-volume-none")}</option>
                <option value="fat32">{t("options-zip-volume-fat32")}</option>
              </select>
            </div>
          ) : null}
          <div class="field">
            <label for={ids.tz}>{t("options-timezone")}</label>
            <select id={ids.tz} value={draft.home_timezone} aria-describedby={`${ids.tz}-desc`} onChange={(e) => set({ home_timezone: (e.currentTarget as HTMLSelectElement).value })}>
              {zones.map((z) => (
                <option key={z} value={z}>
                  {z.replace(/_/g, " ")}
                </option>
              ))}
            </select>
            <p id={`${ids.tz}-desc`} class="field-desc">
              {t("options-timezone-desc")}
            </p>
          </div>
          <div class="field">
            <label for={ids.workers}>{t("settings-workers")}</label>
            <select
              id={ids.workers}
              value={draft.workers === null ? "auto" : String(draft.workers)}
              aria-describedby={`${ids.workers}-desc`}
              onChange={(e) => {
                const v = (e.currentTarget as HTMLSelectElement).value;
                set({ workers: v === "auto" ? null : Number(v) });
              }}
            >
              <option value="auto">{t("settings-workers-auto")}</option>
              {Array.from({ length: Math.min(16, Math.max(2, cores)) }, (_, i) => i + 1).map((n) => (
                <option key={n} value={String(n)}>
                  {t("settings-workers-n", { count: n })}
                </option>
              ))}
            </select>
            <p id={`${ids.workers}-desc`} class="field-desc">
              {t("settings-workers-desc")}
            </p>
          </div>
          <div class="field">
            <label for={ids.template}>{t("settings-template")}</label>
            <input
              id={ids.template}
              type="text"
              class="mono"
              value={draft.output_template}
              spellcheck={false}
              aria-invalid={problem ? "true" : undefined}
              aria-describedby={`${ids.template}-desc`}
              onInput={(e) => set({ output_template: (e.currentTarget as HTMLInputElement).value })}
            />
            <p id={`${ids.template}-desc`} class={problem ? "field-error" : "field-desc"}>
              {problem ? t("settings-template-problem", { problem }) : t("settings-template-example", { example: templateExample(draft.output_template) })}
            </p>
            <p class="field-desc">{t("settings-template-tokens", { tokens: TEMPLATE_TOKENS.join("  ") })}</p>
          </div>
        </section>

        <section class="card" aria-labelledby="s-license">
          <h2 id="s-license" class="card-title">
            {t("settings-license")}
          </h2>
          {license ? (
            <>
              <p>
                <Badge tone={license.tier === "licensed" ? "success" : license.tier === "grace" ? "warning" : "neutral"}>
                  {t("settings-license-tier", { tier: license.tier })}
                </Badge>
              </p>
              {license.key_masked ? <p class="mono">{license.key_masked}</p> : null}
              {license.verified_at ? <p class="field-desc">{t("settings-license-verified", { date: formatInstant(license.verified_at, i18n.formatLocale) })}</p> : null}
              {license.grace_until ? <p class="field-error">{t("settings-license-grace", { date: formatInstant(license.grace_until, i18n.formatLocale) })}</p> : null}
              <div class="row-actions">
                {license.tier === "free" ? (
                  <Button variant="primary" onClick={() => store.openUnlock("header")}>
                    {t("settings-license-unlock")}
                  </Button>
                ) : (
                  <Button onClick={() => setConfirmDeactivate(true)}>{t("settings-license-deactivate")}</Button>
                )}
                <Button variant="ghost" onClick={() => void store.buy()}>
                  <IconExternal size={18} />
                  {t("settings-license-buy-page")}
                </Button>
              </div>
            </>
          ) : null}
        </section>

        <section class="card" aria-labelledby="s-privacy">
          <h2 id="s-privacy" class="card-title">
            {t("settings-privacy")}
          </h2>
          <Checkbox checked={draft.telemetry} onChange={(v) => set({ telemetry: v })} label={t("settings-telemetry")} description={t("settings-telemetry-desc")} />
          <details class="payload">
            <summary>{t("settings-telemetry-preview")}</summary>
            <p class="field-desc">{t("settings-telemetry-preview-desc")}</p>
            <pre class="code-block" tabIndex={0} aria-label={t("settings-telemetry-preview")}>
              {payload ? JSON.stringify(payload, null, 2) : "…"}
            </pre>
          </details>
        </section>

        <section class="card" aria-labelledby="s-updates">
          <h2 id="s-updates" class="card-title">
            {t("settings-updates")}
          </h2>
          <Checkbox checked={draft.update_checks} onChange={(v) => set({ update_checks: v })} label={t("settings-update-checks")} description={t("settings-update-checks-desc")} />
          <div class="field">
            <label for={ids.channel}>{t("settings-channel")}</label>
            <select id={ids.channel} value={draft.update_channel} disabled={!draft.update_checks} onChange={(e) => set({ update_channel: (e.currentTarget as HTMLSelectElement).value as Settings["update_channel"] })}>
              <option value="stable">{t("settings-channel-stable")}</option>
              <option value="beta">{t("settings-channel-beta")}</option>
            </select>
          </div>
        </section>

        <section class="card" aria-labelledby="s-language">
          <h2 id="s-language" class="card-title">
            {t("settings-language-title")}
          </h2>
          <div class="field">
            <label for={ids.language}>{t("settings-language")}</label>
            <select id={ids.language} value={draft.language} aria-describedby={`${ids.language}-desc`} onChange={(e) => set({ language: (e.currentTarget as HTMLSelectElement).value })}>
              <option value="system">{t("settings-language-system")}</option>
              <option value="en-US">{t("settings-language-en-us")}</option>
            </select>
            <p id={`${ids.language}-desc`} class="field-desc">
              {t("settings-language-desc", { locale: i18n.formatLocale })}
            </p>
          </div>
        </section>

        {store.flow.value.screen === "processing" ? <Callout tone="info" title={t("settings-running-note")} /> : null}

        <ActionBar>
          <Button variant="ghost" onClick={() => store.closePage()}>
            <IconArrowBack size={18} />
            {t("page-back")}
          </Button>
          <Button onClick={() => setDraft(saved)} disabled={!dirty}>
            {t("settings-revert")}
          </Button>
          <button type="submit" class="btn btn-primary" disabled={!dirty || !!problem || saving}>
            {t("settings-save")}
          </button>
        </ActionBar>
      </form>

      <Dialog
        open={confirmDeactivate}
        title={t("settings-deactivate-title")}
        onClose={() => setConfirmDeactivate(false)}
        footer={
          <>
            <Button onClick={() => setConfirmDeactivate(false)}>{t("settings-deactivate-keep")}</Button>
            <Button
              variant="danger"
              onClick={() => {
                setConfirmDeactivate(false);
                void store.deactivate();
              }}
            >
              {t("settings-deactivate-confirm")}
            </Button>
          </>
        }
      >
        <p>{t("settings-deactivate-body")}</p>
      </Dialog>
    </div>
  );
}

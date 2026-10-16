import { useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import { byteBaseFor, formatBytes } from "../format/format";
import { Button, Dialog } from "../ui/components";

/** FR-41 / NFR-PRV-06: what Export diagnostics will save, shown before anything is written. */
export function DiagnosticsDialog() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const preview = store.diagnosticsPreview.value;
  const base = byteBaseFor(store.host.platform);
  return (
    <Dialog
      open={preview !== null}
      title={t("diagnostics-title")}
      onClose={() => store.closeDiagnostics()}
      footer={
        <>
          <Button variant="ghost" onClick={() => store.closeDiagnostics()}>
            {t("diagnostics-cancel")}
          </Button>
          <Button variant="primary" onClick={() => void store.exportDiagnostics()} busy={store.diagnosticsBusy.value}>
            {t("diagnostics-save")}
          </Button>
        </>
      }
    >
      {preview ? (
        <>
          <p>{t("diagnostics-intro")}</p>
          <ul class="diag-list">
            {preview.files.map((f) => (
              <li key={f.name}>
                <span class="mono">{f.name}</span>
                <span class="muted">{formatBytes(f.bytes, i18n.formatLocale, base)}</span>
                <span class="diag-kind">{t("diagnostics-kind", { kind: f.kind })}</span>
              </li>
            ))}
          </ul>
          <p class="field-desc">{t("diagnostics-not-included")}</p>
          <p class="field-desc">{t("diagnostics-destination")}</p>
          <p class="path">{preview.destination}</p>
        </>
      ) : null}
    </Dialog>
  );
}

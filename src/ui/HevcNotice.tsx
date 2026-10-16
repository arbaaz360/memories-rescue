import { useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import type { HevcNotice as Notice } from "../ipc/types";
import { Button, Callout } from "./components";

/**
 * FR-51: HEVC videos this computer cannot play. Dismissible for the session,
 * and it offers the one setting that fixes it ("Maximise Windows compatibility").
 */
export function HevcNotice(props: { notice: Notice | null; onEnable: () => void }) {
  const store = useStore();
  const t = useI18n().t;
  if (!props.notice || store.hevcDismissed.value || store.options.value?.windows_compatibility) return null;
  return (
    <Callout
      tone="info"
      title={t("hevc-title")}
      onDismiss={() => {
        store.hevcDismissed.value = true;
      }}
      dismissLabel={t("notice-dismiss")}
      actions={
        <Button variant="primary" onClick={props.onEnable}>
          {t("hevc-enable")}
        </Button>
      }
    >
      <p>{t("hevc-body", { hevc: props.notice.hevc, sampled: props.notice.sampled })}</p>
    </Callout>
  );
}

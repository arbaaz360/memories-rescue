import { useEffect, useId, useState } from "preact/hooks";
import { useI18n } from "../i18n/context";
import { errorMessageId } from "../i18n/l10n";
import { useStore } from "../state/context";
import { Button, Dialog } from "../ui/components";
import { IconCheck, IconExternal } from "../ui/icons";
import { MOCK_VALID_KEY } from "../ipc/mock/engine";

/** Unlock dialog (§7.2, §9.2): price, what you get, Buy on Gumroad, key field, offline note. */
export function UnlockDialog() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const { unlock } = store.flow.value;
  const license = store.license.value;
  const [key, setKey] = useState("");
  const keyId = useId();
  const hintId = `${keyId}-hint`;
  const errorId = `${keyId}-error`;
  const descId = `${keyId}-desc`;
  const error = store.unlockError.value;
  const busy = store.unlockBusy.value;
  const open = unlock !== null;

  useEffect(() => {
    if (!open) setKey("");
  }, [open]);

  if (!license) return null;
  const licensed = license.tier === "licensed";
  const errorText = error ? (i18n.has(errorMessageId(error.code)) ? t(errorMessageId(error.code)) : t("error-unknown")) : null;
  const errorNext = error ? (i18n.has(errorMessageId(error.code)) ? i18n.attr(errorMessageId(error.code), "next") : "") : null;

  const submit = async (e?: Event) => {
    e?.preventDefault();
    if (key.trim().length === 0 || busy) return;
    await store.activate(key);
  };

  return (
    <Dialog
      open={open}
      title={unlock === "free_limit" ? t("unlock-title-limit", { limit: license.free_limit }) : t("unlock-title")}
      onClose={() => void store.later()}
      describedBy={descId}
      footer={
        licensed ? (
          <Button variant="primary" onClick={() => void store.later()}>
            {t("dialog-close")}
          </Button>
        ) : (
          <>
            <Button variant="ghost" onClick={() => void store.later()}>
              {unlock === "free_limit" ? t("unlock-later-limit") : t("unlock-later")}
            </Button>
            <Button onClick={() => void store.buy()}>
              <IconExternal size={18} />
              {t("unlock-buy")}
            </Button>
            <Button variant="primary" onClick={() => void submit()} disabled={key.trim().length === 0} busy={busy}>
              {t("unlock-activate")}
            </Button>
          </>
        )
      }
    >
      {licensed ? (
        <p id={descId}>{t("unlock-already", { key: license.key_masked ?? "" })}</p>
      ) : (
        <>
          <p id={descId} class="lede">
            {unlock === "free_limit" ? t("unlock-lede-limit", { limit: license.free_limit }) : t("unlock-lede")}
          </p>
          <div class="price-row">
            <span class="price">{license.price_display ?? t("unlock-price-pending")}</span>
            <span class="muted">{t("unlock-price-note")}</span>
          </div>
          <h3 class="subhead">{t("unlock-get-title")}</h3>
          <ul class="check-list">
            {(["unlock-get-all", "unlock-get-updates", "unlock-get-offline", "unlock-get-computers"] as const).map((id) => (
              <li key={id}>
                <IconCheck size={18} />
                {t(id)}
              </li>
            ))}
          </ul>
          <p class="field-desc">{t("unlock-buy-note")}</p>
          <form class="field" onSubmit={(e) => void submit(e)}>
            <label for={keyId}>{t("unlock-key-label")}</label>
            <input
              id={keyId}
              type="text"
              value={key}
              autoComplete="off"
              spellcheck={false}
              autoCapitalize="characters"
              aria-invalid={error ? "true" : undefined}
              aria-describedby={error ? `${errorId} ${hintId}` : hintId}
              placeholder={t("unlock-key-placeholder")}
              onInput={(e) => setKey((e.currentTarget as HTMLInputElement).value)}
            />
            {error ? (
              <p id={errorId} class="field-error" role="alert">
                <span class="field-error-title">{errorText}</span>
                <span>{errorNext}</span>
              </p>
            ) : null}
            <p id={hintId} class="field-desc">
              {t("unlock-offline-note")}
            </p>
            {import.meta.env.DEV && store.backend.kind === "mock" ? <p class="dev-hint">{t("dev-mock-key", { key: MOCK_VALID_KEY })}</p> : null}
          </form>
        </>
      )}
    </Dialog>
  );
}

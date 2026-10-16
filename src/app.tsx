import { I18nContext, useT } from "./i18n/context";
import { errorMessageId } from "./i18n/l10n";
import { StoreContext, useStore } from "./state/context";
import { STEPS, type Screen } from "./state/flow";
import type { AppStore } from "./state/store";
import { Button, Callout } from "./ui/components";
import { IconGear, IconHelp, IconKey, IconLock } from "./ui/icons";
import { Welcome } from "./screens/Welcome";
import { AddFiles } from "./screens/AddFiles";
import { Summary } from "./screens/Summary";
import { Options } from "./screens/Options";
import { Preview } from "./screens/Preview";
import { Processing } from "./screens/Processing";
import { Results } from "./screens/Results";
import { UnlockDialog } from "./dialogs/UnlockDialog";
import { DiagnosticsDialog } from "./dialogs/DiagnosticsDialog";
import { SettingsPage } from "./pages/Settings";
import { AboutPage } from "./pages/About";

const SCREENS: Record<Screen, () => preact.JSX.Element | null> = {
  welcome: Welcome,
  add_files: AddFiles,
  summary: Summary,
  options: Options,
  preview: Preview,
  processing: Processing,
  results: Results,
};

function Logo() {
  return (
    <svg class="logo" width="28" height="28" viewBox="0 0 32 32" aria-hidden="true" focusable="false">
      <rect x="3" y="6" width="20" height="16" rx="3" class="logo-back" />
      <rect x="9" y="10" width="20" height="16" rx="3" class="logo-front" />
      <path d="M13 22l4-5 3 3.5 2-2.5 3 4z" class="logo-mark" />
      <circle cx="23.5" cy="14.5" r="1.8" class="logo-mark" />
    </svg>
  );
}

function Steps() {
  const store = useStore();
  const t = useT();
  const { screen, page } = store.flow.value;
  if (screen === "welcome" || page) return null;
  const current = STEPS.indexOf(screen);
  return (
    <nav class="steps" aria-label={t("steps-label")}>
      <ol>
        {STEPS.map((s, i) => (
          <li
            key={s}
            class={i < current ? "step step-done" : i === current ? "step step-current" : "step"}
            aria-current={i === current ? "step" : undefined}
          >
            <span class="step-num" aria-hidden="true">
              {i + 1}
            </span>
            <span class="step-label">{t(`step-${s.replace("_", "-")}`)}</span>
            {i < current ? <span class="visually-hidden">{t("steps-done-suffix")}</span> : null}
          </li>
        ))}
      </ol>
    </nav>
  );
}

function LicensePill() {
  const store = useStore();
  const t = useT();
  const lic = store.license.value;
  if (!lic) return null;
  if (lic.tier === "licensed") {
    return (
      <button type="button" class="pill pill-ok" onClick={() => store.openPage("settings")}>
        <IconKey size={16} />
        {t("header-licensed")}
      </button>
    );
  }
  return (
    <button type="button" class={lic.tier === "grace" ? "pill pill-warn" : "pill"} onClick={() => store.openUnlock("header")}>
      <IconLock size={16} />
      {lic.tier === "grace" ? t("header-grace") : t("header-free", { limit: lic.free_limit })}
    </button>
  );
}

function MockBadge() {
  const store = useStore();
  const t = useT();
  if (!import.meta.env.DEV || store.backend.kind !== "mock") return null;
  return (
    <span class="mock-badge" title={store.boot.detail ?? undefined}>
      {t("dev-mock-badge", { reason: store.boot.reason })}
    </span>
  );
}

/** §10.2: an update the feed offers; installs only on the user's word and never during a job. */
function UpdateNotice() {
  const store = useStore();
  const t = useT();
  const u = store.update.value;
  if (!u) return null;
  const running = store.jobRunning.value;
  return (
    <Callout
      tone="info"
      title={t("update-title", { version: u.version })}
      onDismiss={() => store.dismissUpdate()}
      dismissLabel={t("update-later")}
      actions={
        <>
          <Button variant="primary" onClick={() => void store.installUpdate()} disabled={running} busy={store.updateBusy.value}>
            {t("update-install")}
          </Button>
          <Button variant="ghost" onClick={() => store.dismissUpdate()}>
            {t("update-later")}
          </Button>
        </>
      }
    >
      <p>{running ? t("update-wait-for-job") : t("update-body", { current: u.current_version, channel: u.channel })}</p>
      {u.notes ? (
        <details>
          <summary>{t("update-notes")}</summary>
          <p>{u.notes}</p>
        </details>
      ) : null}
      {store.updateError.value ? <p class="field-error">{t("update-failed")}</p> : null}
    </Callout>
  );
}

function Notices() {
  const store = useStore();
  const t = useT();
  const list = store.notices.value;
  if (list.length === 0 && !store.update.value) return null;
  return (
    <div class="notices">
      <UpdateNotice />
      {list.map((n) => (
        <Callout
          key={n.code}
          tone="warning"
          title={t(errorMessageId(n.code))}
          onDismiss={() => store.dismissNotice(n.code)}
          dismissLabel={t("notice-dismiss")}
        >
          <p>{n.message}</p>
        </Callout>
      ))}
    </div>
  );
}

function Shell() {
  const store = useStore();
  const t = useT();
  const { screen, page } = store.flow.value;
  const Current = SCREENS[screen];
  const announcement = store.announcement.value;
  return (
    <div class="app">
      <a class="skip-link" href="#main">
        {t("skip-to-content")}
      </a>
      <header class="topbar">
        <div class="brand">
          <Logo />
          <span class="brand-name">{t("app-name")}</span>
          <MockBadge />
        </div>
        <nav class="topnav" aria-label={t("topnav-label")}>
          <LicensePill />
          <button
            type="button"
            class={page === "settings" ? "nav-btn nav-btn-current" : "nav-btn"}
            aria-current={page === "settings" ? "page" : undefined}
            onClick={() => (page === "settings" ? store.closePage() : store.openPage("settings"))}
          >
            <IconGear size={18} />
            {t("nav-settings")}
          </button>
          <button
            type="button"
            class={page === "about" ? "nav-btn nav-btn-current" : "nav-btn"}
            aria-current={page === "about" ? "page" : undefined}
            onClick={() => (page === "about" ? store.closePage() : store.openPage("about"))}
          >
            <IconHelp size={18} />
            {t("nav-about")}
          </button>
        </nav>
      </header>
      <Steps />
      <main id="main" class="main" tabIndex={-1}>
        <Notices />
        {page === "settings" ? <SettingsPage /> : page === "about" ? <AboutPage /> : <Current key={screen} />}
      </main>
      <UnlockDialog />
      <DiagnosticsDialog />
      <div class="visually-hidden" aria-live="polite" aria-atomic="true">
        <span key={announcement.n}>{announcement.text}</span>
      </div>
    </div>
  );
}

export function App(props: { store: AppStore }) {
  const i18n = props.store.i18n.value;
  return (
    <StoreContext.Provider value={props.store}>
      <I18nContext.Provider value={i18n}>
        <Shell />
      </I18nContext.Provider>
    </StoreContext.Provider>
  );
}

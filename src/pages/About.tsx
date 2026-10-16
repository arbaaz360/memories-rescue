import { useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import { ActionBar, Button, ScreenTitle } from "../ui/components";
import { IconArrowBack, IconExternal, IconFile, IconMail } from "../ui/icons";

/** About: version, licences, third-party notices, ffmpeg source, disclaimer, support (§7.2, NFR-LEG-01/02). */
export function AboutPage() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const info = store.appInfo.value;
  const sidecars = store.sidecars.value;

  return (
    <div class="screen page">
      <ScreenTitle lede={t("about-lede")}>{t("about-title")}</ScreenTitle>
      {info ? (
        <>
          <section class="card" aria-labelledby="a-version">
            <h2 id="a-version" class="card-title">
              {t("about-version-title")}
            </h2>
            <dl class="kv">
              <dt>{t("about-version")}</dt>
              <dd class="mono">{info.version}</dd>
              <dt>{t("about-publisher")}</dt>
              <dd>{t("about-publisher-name")}</dd>
              <dt>{t("about-tools")}</dt>
              <dd>
                {sidecars?.status === "ok" ? (
                  <>
                    <span class="mono block">{sidecars.versions.ffmpeg}</span>
                    <span class="mono block">{t("about-exiftool", { version: sidecars.versions.exiftool })}</span>
                  </>
                ) : sidecars?.status === "failed" ? (
                  <span class="field-error">{t("about-tools-failed", { message: sidecars.message })}</span>
                ) : (
                  <span class="muted">{t("about-tools-unavailable")}</span>
                )}
              </dd>
            </dl>
          </section>

          <section class="card" aria-labelledby="a-legal">
            <h2 id="a-legal" class="card-title">
              {t("about-legal-title")}
            </h2>
            <p>{t("about-disclaimer")}</p>
            <p>{t("about-ffmpeg")}</p>
            <div class="row-actions">
              <Button onClick={() => void store.openPath(info.licenses_path)}>
                <IconFile size={18} />
                {t("about-licenses")}
              </Button>
              <Button onClick={() => void store.openPath(info.third_party_notices_path)}>
                <IconFile size={18} />
                {t("about-notices")}
              </Button>
              <Button onClick={() => void store.host.openUrl(info.ffmpeg_source_url)}>
                <IconExternal size={18} />
                {t("about-ffmpeg-source")}
              </Button>
              <Button onClick={() => void store.host.openUrl(info.privacy_policy_url)}>
                <IconExternal size={18} />
                {t("about-privacy")}
              </Button>
              <Button onClick={() => void store.host.openUrl(info.terms_url)}>
                <IconExternal size={18} />
                {t("about-terms")}
              </Button>
              <Button onClick={() => void store.host.openUrl(info.faq_url)}>
                <IconExternal size={18} />
                {t("about-faq")}
              </Button>
            </div>
          </section>

          <section class="card" aria-labelledby="a-support">
            <h2 id="a-support" class="card-title">
              {t("about-support-title")}
            </h2>
            <p>{t("about-support-body")}</p>
            <div class="row-actions">
              <Button onClick={() => void store.host.openUrl(`mailto:${info.support_email}`)}>
                <IconMail size={18} />
                {info.support_email}
              </Button>
              <Button variant="ghost" onClick={() => void store.host.openUrl(info.website_url)}>
                <IconExternal size={18} />
                {t("about-website")}
              </Button>
            </div>
          </section>
        </>
      ) : null}
      <ActionBar>
        <Button variant="ghost" onClick={() => store.closePage()}>
          <IconArrowBack size={18} />
          {t("page-back")}
        </Button>
      </ActionBar>
    </div>
  );
}

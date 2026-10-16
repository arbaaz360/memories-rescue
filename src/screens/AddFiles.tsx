import { useEffect } from "preact/hooks";
import { useI18n } from "../i18n/context";
import { useStore } from "../state/context";
import { byteBaseFor, formatBytes } from "../format/format";
import type { InputFile } from "../ipc/types";
import { ActionBar, Badge, Button, Callout, ErrorCallout, ScreenTitle } from "../ui/components";
import { IconFile, IconFolder, IconUpload, IconZip, IconArrowBack } from "../ui/icons";

function RoleBadge(props: { input: InputFile }) {
  const t = useI18n().t;
  const { role, part_number } = props.input;
  if (role === "primary") return <Badge tone="success">{t("inputs-role-primary")}</Badge>;
  if (role === "part") return <Badge tone="info">{t("inputs-role-part", { n: part_number ?? 0 })}</Badge>;
  if (role === "ignored") return <Badge>{t("inputs-role-ignored")}</Badge>;
  return <Badge>{t("inputs-role-unknown")}</Badge>;
}

function StatusBadge(props: { input: InputFile }) {
  const t = useI18n().t;
  switch (props.input.status) {
    case "ok":
      return <Badge tone="success" icon>{t("inputs-status-ok")}</Badge>;
    case "pending":
      return <Badge>{t("inputs-status-pending")}</Badge>;
    case "truncated":
      return <Badge tone="warning" icon>{t("inputs-status-truncated")}</Badge>;
    case "not_export":
      return <Badge tone="danger" icon>{t("inputs-status-not-export")}</Badge>;
    case "duplicate":
      return <Badge>{t("inputs-status-duplicate")}</Badge>;
  }
}

/** S1: drop zone, pickers, the list of inputs with size and detected role. */
export function AddFiles() {
  const store = useStore();
  const i18n = useI18n();
  const t = i18n.t;
  const base = byteBaseFor(store.host.platform);
  const inputs = store.inputs.value;
  const usable = store.usableInputs.value;
  const ignored = store.ignoredNotice.value;
  const dragging = store.dragging.value;

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let alive = true;
    void store.host
      .onDragDrop((e) => {
        if (e.type === "enter" || e.type === "over") store.dragging.value = true;
        else if (e.type === "leave") store.dragging.value = false;
        else {
          store.dragging.value = false;
          void store.addPaths(e.paths);
        }
      })
      .then((u) => {
        if (alive) unlisten = u;
        else u();
      });
    return () => {
      alive = false;
      unlisten?.();
      store.dragging.value = false;
    };
  }, [store]);

  const totalBytes = usable.reduce((n, i) => n + i.size_bytes, 0);
  const sorted = [...inputs].sort((a, b) => (a.part_number ?? 99) - (b.part_number ?? 99) || a.name.localeCompare(b.name));

  return (
    <div class="screen">
      <ScreenTitle lede={t("inputs-lede")}>{t("inputs-title")}</ScreenTitle>

      <div class={dragging ? "dropzone dropzone-active" : "dropzone"}>
        <IconUpload size={40} class="dropzone-icon" />
        <p class="dropzone-title">{dragging ? t("inputs-drop-release") : t("inputs-drop-title")}</p>
        <p class="dropzone-hint">{t("inputs-drop-hint")}</p>
        <div class="dropzone-actions">
          <Button variant="primary" onClick={() => void store.addFiles()} busy={store.inputsBusy.value}>
            <IconZip size={18} />
            {t("inputs-add-files")}
          </Button>
          <Button onClick={() => void store.addFolders()} disabled={store.inputsBusy.value}>
            <IconFolder size={18} />
            {t("inputs-add-folder")}
          </Button>
        </div>
      </div>

      {ignored.length > 0 ? (
        <Callout tone="warning" title={t("inputs-ignored-title", { count: ignored.length })} role="status">
          <p>{t("inputs-ignored-body", { count: ignored.length, names: new Intl.ListFormat(i18n.formatLocale).format(ignored) })}</p>
        </Callout>
      ) : null}

      {store.inputsError.value ? <ErrorCallout error={store.inputsError.value} /> : null}

      {inputs.length > 0 ? (
        <section aria-labelledby="inputs-list-title">
          <div class="list-head">
            <h2 id="inputs-list-title">{t("inputs-list-title")}</h2>
            <p class="list-meta">{t("inputs-list-meta", { count: usable.length, size: formatBytes(totalBytes, i18n.formatLocale, base) })}</p>
          </div>
          <ul class="file-list">
            {sorted.map((input) => (
              <li key={input.id} class={input.status === "not_export" ? "file-row file-row-muted" : "file-row"}>
                <span class="file-icon">
                  {input.kind === "folder" ? <IconFolder /> : input.kind === "zip" ? <IconZip /> : <IconFile />}
                </span>
                <div class="file-main">
                  <p class="file-name">{input.name}</p>
                  <p class="file-meta">
                    <span>{formatBytes(input.size_bytes, i18n.formatLocale, base)}</span>
                    <RoleBadge input={input} />
                    <StatusBadge input={input} />
                  </p>
                </div>
                <Button variant="ghost" onClick={() => void store.removeInput(input)} aria-label={t("inputs-remove-label", { name: input.name })}>
                  {t("inputs-remove")}
                </Button>
              </li>
            ))}
          </ul>
        </section>
      ) : (
        <p class="empty-hint">{t("inputs-empty")}</p>
      )}

      <ActionBar>
        <Button variant="ghost" onClick={() => store.back()}>
          <IconArrowBack size={18} />
          {t("action-back")}
        </Button>
        <Button variant="primary" onClick={() => store.continueFromInputs()} disabled={usable.length === 0}>
          {t("action-continue")}
        </Button>
      </ActionBar>
    </div>
  );
}

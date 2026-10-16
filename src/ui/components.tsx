import type { ButtonHTMLAttributes, ComponentChildren, JSX } from "preact";
import { useEffect, useId, useLayoutEffect, useRef } from "preact/hooks";
import { useI18n, useT } from "../i18n/context";
import { errorMessageId } from "../i18n/l10n";
import type { IpcError } from "../ipc/types";
import { formatPercent } from "../format/format";
import { IconClose, IconError, IconInfo, IconWarning, IconCheck } from "./icons";

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary" | "ghost" | "danger";
  busy?: boolean;
  disabled?: boolean;
};

/**
 * While `busy`, the button stays focusable (aria-disabled, clicks ignored):
 * a real `disabled` would drop keyboard focus to the page mid-action.
 */
export function Button({ variant = "secondary", busy, children, class: cls, disabled, onClick, ...rest }: ButtonProps) {
  const classes = ["btn", `btn-${variant}`, busy ? "btn-busy" : null, cls].filter(Boolean).join(" ");
  return (
    <button
      type="button"
      class={classes}
      disabled={disabled}
      aria-disabled={busy ? "true" : undefined}
      aria-busy={busy ? "true" : undefined}
      onClick={busy ? undefined : onClick}
      {...rest}
    >
      {busy ? <Spinner /> : null}
      {children}
    </button>
  );
}

export function Spinner() {
  return <span class="spinner" aria-hidden="true" />;
}

/** The screen title. Takes focus when the screen opens, so screen readers announce where the user is. */
export function ScreenTitle(props: { children: ComponentChildren; lede?: ComponentChildren; eyebrow?: ComponentChildren }) {
  const ref = useRef<HTMLHeadingElement>(null);
  // Layout effect: focus lands before the first paint of the screen, so neither
  // a screen reader nor a fast Tab press ever sees the previous focus.
  useLayoutEffect(() => {
    ref.current?.focus({ preventScroll: false });
  }, []);
  return (
    <header class="screen-header">
      {props.eyebrow ? <p class="eyebrow">{props.eyebrow}</p> : null}
      <h1 ref={ref} tabIndex={-1} class="screen-title">
        {props.children}
      </h1>
      {props.lede ? <p class="lede">{props.lede}</p> : null}
    </header>
  );
}

export function ActionBar(props: { children: ComponentChildren; label?: string }) {
  return (
    <div class="action-bar" role="group" aria-label={props.label}>
      {props.children}
    </div>
  );
}

type Tone = "info" | "warning" | "danger" | "success";

const toneIcon: Record<Tone, () => JSX.Element> = {
  info: () => <IconInfo />,
  warning: () => <IconWarning />,
  danger: () => <IconError />,
  success: () => <IconCheck />,
};

export function Callout(props: {
  tone: Tone;
  title?: ComponentChildren;
  children?: ComponentChildren;
  actions?: ComponentChildren;
  role?: "status" | "alert";
  onDismiss?: () => void;
  dismissLabel?: string;
}) {
  const titleId = useId();
  return (
    <section class={`callout callout-${props.tone}`} role={props.role} aria-labelledby={props.title ? titleId : undefined}>
      <span class="callout-icon">{toneIcon[props.tone]()}</span>
      <div class="callout-body">
        {props.title ? (
          <h3 id={titleId} class="callout-title">
            {props.title}
          </h3>
        ) : null}
        {props.children ? <div class="callout-text">{props.children}</div> : null}
        {props.actions ? <div class="callout-actions">{props.actions}</div> : null}
      </div>
      {props.onDismiss ? (
        <button type="button" class="icon-btn" onClick={props.onDismiss} aria-label={props.dismissLabel}>
          <IconClose size={18} />
        </button>
      ) : null}
    </section>
  );
}

/** Plain-language copy for an engine error (NFR-USE-02), raw detail on request. */
export function ErrorCallout(props: { error: IpcError; args?: Record<string, string | number>; actions?: ComponentChildren; tone?: Tone }) {
  const i18n = useI18n();
  const id = errorMessageId(props.error.code);
  const title = i18n.has(id) ? i18n.t(id, props.args) : i18n.t("error-unknown");
  const next = i18n.has(id) ? i18n.attr(id, "next", props.args) : i18n.attr("error-unknown", "next");
  const detail = [props.error.message, props.error.detail].filter((s) => !!s);
  return (
    <Callout tone={props.tone ?? "danger"} title={title} role="alert" actions={props.actions}>
      <p>{next}</p>
      <details class="error-detail">
        <summary>{i18n.t("error-detail-summary")}</summary>
        <p>
          <code>{props.error.code}</code>
        </p>
        {detail.map((d, i) => (
          <p key={i}>{d}</p>
        ))}
      </details>
    </Callout>
  );
}

export function Badge(props: { tone?: Tone | "neutral"; children: ComponentChildren; icon?: boolean }) {
  const tone = props.tone ?? "neutral";
  return (
    <span class={`badge badge-${tone}`}>
      {props.icon && tone !== "neutral" ? toneIcon[tone]() : null}
      {props.children}
    </span>
  );
}

export function ProgressBar(props: { pct: number; label: string; valueText?: string; indeterminate?: boolean }) {
  const i18n = useI18n();
  const pct = Math.min(100, Math.max(0, props.pct));
  return (
    <div
      class={props.indeterminate ? "progress progress-indeterminate" : "progress"}
      role="progressbar"
      aria-label={props.label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={props.indeterminate ? undefined : Math.round(pct)}
      aria-valuetext={props.indeterminate ? undefined : (props.valueText ?? formatPercent(pct, i18n.formatLocale))}
    >
      <div class="progress-fill" style={{ inlineSize: props.indeterminate ? undefined : `${pct}%` }} />
    </div>
  );
}

/**
 * Free space on the destination with the estimate drawn into it. `role=meter`
 * reports the share of free space the job needs.
 */
export function SpaceMeter(props: {
  label: string;
  usedPct: number;
  needPct: number;
  valueText: string;
  sufficient: boolean;
  describedBy?: string;
}) {
  const used = Math.min(100, Math.max(0, props.usedPct));
  const need = Math.min(100 - used, Math.max(0, props.needPct));
  return (
    <div
      class={props.sufficient ? "meter" : "meter meter-short"}
      role="meter"
      aria-label={props.label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(used + need)}
      aria-valuetext={props.valueText}
      aria-describedby={props.describedBy}
    >
      <div class="meter-used" style={{ inlineSize: `${used}%` }} />
      <div class="meter-need" style={{ inlineSize: `${need}%` }} />
    </div>
  );
}

export function Checkbox(props: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: ComponentChildren;
  description?: ComponentChildren;
  disabled?: boolean;
}) {
  const id = useId();
  const descId = `${id}-desc`;
  return (
    <div class={props.disabled ? "check check-disabled" : "check"}>
      <input
        id={id}
        type="checkbox"
        checked={props.checked}
        disabled={props.disabled}
        aria-describedby={props.description ? descId : undefined}
        onChange={(e) => props.onChange((e.currentTarget as HTMLInputElement).checked)}
      />
      <div>
        <label for={id}>{props.label}</label>
        {props.description ? (
          <p id={descId} class="field-desc">
            {props.description}
          </p>
        ) : null}
      </div>
    </div>
  );
}

export function RadioGroup<T extends string>(props: {
  legend: ComponentChildren;
  value: T;
  options: { value: T; label: ComponentChildren; description?: ComponentChildren }[];
  onChange: (value: T) => void;
  name: string;
}) {
  const id = useId();
  return (
    <fieldset class="radio-group">
      <legend>{props.legend}</legend>
      {props.options.map((o) => {
        const oid = `${id}-${o.value}`;
        return (
          <div class="radio" key={o.value}>
            <input
              id={oid}
              type="radio"
              name={props.name}
              value={o.value}
              checked={props.value === o.value}
              aria-describedby={o.description ? `${oid}-desc` : undefined}
              onChange={() => props.onChange(o.value)}
            />
            <div>
              <label for={oid}>{o.label}</label>
              {o.description ? (
                <p id={`${oid}-desc`} class="field-desc">
                  {o.description}
                </p>
              ) : null}
            </div>
          </div>
        );
      })}
    </fieldset>
  );
}

/**
 * Modal dialog on the native <dialog> element: focus moves in and is kept
 * inside, the page behind is inert, Esc closes, focus returns to the opener.
 */
export function Dialog(props: {
  open: boolean;
  title: ComponentChildren;
  onClose: () => void;
  children: ComponentChildren;
  footer?: ComponentChildren;
  size?: "md" | "lg";
  describedBy?: string;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const t = useT();
  const titleId = useId();
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (props.open && !d.open) d.showModal();
    if (!props.open && d.open) d.close();
  }, [props.open]);
  return (
    <dialog
      ref={ref}
      class={`dialog dialog-${props.size ?? "md"}`}
      aria-labelledby={titleId}
      aria-describedby={props.describedBy}
      onCancel={(e) => {
        e.preventDefault();
        props.onClose();
      }}
    >
      <div class="dialog-head">
        <h2 id={titleId} class="dialog-title">
          {props.title}
        </h2>
        <button type="button" class="icon-btn" onClick={props.onClose} aria-label={t("dialog-close")}>
          <IconClose />
        </button>
      </div>
      <div class="dialog-body">{props.children}</div>
      {props.footer ? <div class="dialog-foot">{props.footer}</div> : null}
    </dialog>
  );
}

export function Stat(props: { label: ComponentChildren; value: ComponentChildren; icon?: JSX.Element; hint?: ComponentChildren }) {
  return (
    <div class="stat">
      <dt class="stat-label">
        {props.icon}
        {props.label}
      </dt>
      <dd class="stat-value">{props.value}</dd>
      {props.hint ? <dd class="stat-hint">{props.hint}</dd> : null}
    </div>
  );
}

export function Loading(props: { label: ComponentChildren }) {
  return (
    <div class="loading" role="status">
      <Spinner />
      <span>{props.label}</span>
    </div>
  );
}

/** Hidden from sight, read by screen readers. */
export function VisuallyHidden(props: { children: ComponentChildren }) {
  return <span class="visually-hidden">{props.children}</span>;
}

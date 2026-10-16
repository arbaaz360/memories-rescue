import { createContext, type ComponentChildren, type VNode } from "preact";
import { useContext } from "preact/hooks";
import { parseRich, type I18n, type MessageArgs } from "./l10n";

export const I18nContext = createContext<I18n | null>(null);

export function useI18n(): I18n {
  const i18n = useContext(I18nContext);
  if (!i18n) throw new Error("I18nContext missing");
  return i18n;
}

/** Shorthand used by every component. */
export function useT(): I18n["t"] {
  return useI18n().t;
}

type TagRenderer = (children: ComponentChildren) => VNode;

const DEFAULT_TAGS: Record<string, TagRenderer> = {
  b: (c) => <strong>{c}</strong>,
  em: (c) => <em>{c}</em>,
  code: (c) => <code>{c}</code>,
};

/**
 * Renders a message whose text carries `<b>…</b>`-style markup, mapping each
 * tag to an element. Lets translators keep emphasis and links inside one
 * sentence instead of the UI gluing fragments together.
 */
export function Rich(props: { id: string; args?: MessageArgs; tags?: Record<string, TagRenderer> }) {
  const i18n = useI18n();
  const tags = { ...DEFAULT_TAGS, ...props.tags };
  return (
    <>
      {parseRich(i18n.t(props.id, props.args)).map((p, i) => {
        if ("tag" in p) {
          const render = tags[p.tag];
          return render ? <span key={i}>{render(p.text)}</span> : <span key={i}>{p.text}</span>;
        }
        return <span key={i}>{p.text}</span>;
      })}
    </>
  );
}

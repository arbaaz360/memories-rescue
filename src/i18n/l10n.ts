/**
 * Fluent localisation. Every user-facing string lives in
 * `src/locales/<lang>/*.ftl`; UI code asks for a message id with arguments and
 * never joins strings (NFR-I18N-01).
 *
 * Two locales are in play. The UI language picks the bundle and its plural
 * rules (only en-US ships at v1.0). The formatting locale is the user's own
 * (system or chosen) and decides how numbers and dates look (NFR-I18N-03): a
 * British user reads "4 Jul 2019" and a German one "4.812" in English text.
 * `LocaleNumber` carries that split into Fluent: selection uses the bundle's
 * plural rules, display uses the user's locale.
 */

import { FluentBundle, FluentNumber, FluentResource, type FluentVariable } from "@fluent/bundle";

const sources = import.meta.glob("../locales/*/*.ftl", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

/** Languages with a complete catalogue. */
export const AVAILABLE_LANGUAGES = ["en-US"] as const;
export const DEFAULT_LANGUAGE = "en-US";

/** Directions per UI language, for `<html dir>`. Ready for Arabic later (NFR-I18N-02). */
const RTL_LANGUAGES = new Set(["ar", "he", "fa", "ur"]);

export class LocaleNumber extends FluentNumber {
  private readonly formatter: Intl.NumberFormat;

  constructor(value: number, formatLocale: string, opts: Intl.NumberFormatOptions = {}) {
    super(value, opts);
    this.formatter = new Intl.NumberFormat(formatLocale, opts);
  }

  override toString(): string {
    return this.formatter.format(this.value);
  }
}

export type MessageArg = string | number | LocaleNumber | null | undefined;
export type MessageArgs = Record<string, MessageArg>;

export interface I18n {
  /** UI language of the bundle, e.g. "en-US". */
  readonly language: string;
  /** Locale for Intl formatting, e.g. "en-GB". */
  readonly formatLocale: string;
  readonly dir: "ltr" | "rtl";
  t(id: string, args?: MessageArgs): string;
  /** A message attribute, e.g. `.aria-label`. */
  attr(id: string, attribute: string, args?: MessageArgs): string;
  has(id: string): boolean;
  /** A number with explicit Intl options, for arguments that need them. */
  num(value: number, opts?: Intl.NumberFormatOptions): LocaleNumber;
}

function filesFor(language: string): string[] {
  const prefix = `../locales/${language}/`;
  return Object.keys(sources)
    .filter((k) => k.startsWith(prefix))
    .sort();
}

/** Loads one language's catalogue; parse errors are returned, not thrown. */
export function loadBundle(language: string): { bundle: FluentBundle; errors: string[] } {
  const bundle = new FluentBundle(language, { useIsolating: false });
  const errors: string[] = [];
  for (const file of filesFor(language)) {
    const resource = new FluentResource(sources[file]);
    for (const err of bundle.addResource(resource)) errors.push(`${file}: ${err.message}`);
  }
  return { bundle, errors };
}

/** Message ids per file, for the catalogue tests. */
export function catalogueIds(language: string): Map<string, string> {
  const ids = new Map<string, string>();
  for (const file of filesFor(language)) {
    const resource = new FluentResource(sources[file]);
    for (const entry of resource.body) {
      if (!entry.id.startsWith("-")) ids.set(entry.id, file);
    }
  }
  return ids;
}

function canonical(tag: string): string | null {
  try {
    return Intl.getCanonicalLocales(tag)[0] ?? null;
  } catch {
    return null;
  }
}

/** Picks the UI language and the formatting locale from the setting and the system's list. */
export function negotiate(
  setting: string,
  systemLocales: readonly string[],
): { language: string; formatLocale: string } {
  const requested = (setting === "system" || !setting ? [...systemLocales] : [setting])
    .map(canonical)
    .filter((t): t is string => t !== null);
  const formatLocale = requested[0] ?? DEFAULT_LANGUAGE;
  let language: string = DEFAULT_LANGUAGE;
  for (const tag of requested) {
    const exact = AVAILABLE_LANGUAGES.find((a) => a === tag);
    const sameLanguage = AVAILABLE_LANGUAGES.find((a) => a.split("-")[0] === tag.split("-")[0]);
    if (exact ?? sameLanguage) {
      language = (exact ?? sameLanguage) as string;
      break;
    }
  }
  return { language, formatLocale };
}

export function createI18n(setting: string, systemLocales: readonly string[]): I18n {
  const { language, formatLocale } = negotiate(setting, systemLocales);
  const { bundle, errors } = loadBundle(language);
  if (errors.length > 0 && import.meta.env.DEV) console.error("Fluent parse errors", errors);

  const toArgs = (args?: MessageArgs): Record<string, FluentVariable> | undefined => {
    if (!args) return undefined;
    const out: Record<string, FluentVariable> = {};
    for (const [k, v] of Object.entries(args)) {
      if (v === null || v === undefined) out[k] = "";
      else if (typeof v === "number") out[k] = new LocaleNumber(v, formatLocale);
      else out[k] = v;
    }
    return out;
  };

  const report = (id: string, errs: Error[]) => {
    if (errs.length > 0 && import.meta.env.DEV) console.warn(`Fluent: ${id}`, errs.map((e) => e.message));
  };

  return {
    language,
    formatLocale,
    dir: RTL_LANGUAGES.has(language.split("-")[0]) ? "rtl" : "ltr",
    t(id, args) {
      const msg = bundle.getMessage(id);
      if (!msg?.value) {
        if (import.meta.env.DEV) console.warn(`Missing message: ${id}`);
        return id;
      }
      const errs: Error[] = [];
      const out = bundle.formatPattern(msg.value, toArgs(args), errs);
      report(id, errs);
      return out;
    },
    attr(id, attribute, args) {
      const pattern = bundle.getMessage(id)?.attributes[attribute];
      if (!pattern) {
        if (import.meta.env.DEV) console.warn(`Missing attribute: ${id}.${attribute}`);
        return id;
      }
      const errs: Error[] = [];
      const out = bundle.formatPattern(pattern, toArgs(args), errs);
      report(id, errs);
      return out;
    },
    has(id) {
      return bundle.hasMessage(id);
    },
    num(value, opts) {
      return new LocaleNumber(value, formatLocale, opts);
    },
  };
}

/** Splits a translated string with simple `<tag>text</tag>` markup into pieces. */
export type RichPiece = { text: string } | { tag: string; text: string };

export function parseRich(input: string): RichPiece[] {
  const pieces: RichPiece[] = [];
  const re = /<([a-z][a-z0-9]*)>([\s\S]*?)<\/\1>/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(input)) !== null) {
    if (m.index > last) pieces.push({ text: input.slice(last, m.index) });
    pieces.push({ tag: m[1], text: m[2] });
    last = m.index + m[0].length;
  }
  if (last < input.length) pieces.push({ text: input.slice(last) });
  return pieces;
}

/** Message id holding the plain-language copy for an error code (errors.ftl). */
export function errorMessageId(code: string): string {
  return `error-${code.toLowerCase().replace(/_/g, "-")}`;
}

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { ERROR_CODES } from "../ipc/types";
import { catalogueIds, createI18n, errorMessageId, loadBundle, negotiate, parseRich } from "./l10n";

const SRC = join(__dirname, "..");

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...sourceFiles(p));
    else if (/\.(ts|tsx)$/.test(name) && !/\.test\.tsx?$/.test(name)) out.push(p);
  }
  return out;
}

describe("en-US catalogue", () => {
  it("parses without errors", () => {
    expect(loadBundle("en-US").errors).toEqual([]);
  });

  it("has every message id the UI code asks for", () => {
    const ids = catalogueIds("en-US");
    const missing: string[] = [];
    const patterns = [/\bt\("([a-z0-9-]+)"/g, /attr\("([a-z0-9-]+)"/g, /Rich id="([a-z0-9-]+)"/g, /"(unlock-get-[a-z]+)"/g, /"(summary-path-[a-z-]+)"/g];
    for (const file of sourceFiles(SRC)) {
      const text = readFileSync(file, "utf-8");
      for (const re of patterns) {
        for (const m of text.matchAll(re)) if (!ids.has(m[1])) missing.push(`${m[1]} (${file.slice(SRC.length + 1)})`);
      }
    }
    // Ids built at runtime from a fixed set of values.
    const dynamic = [
      ...["add-files", "summary", "options", "preview", "processing", "results"].map((s) => `step-${s}`),
      ...[1, 2, 3, 4].flatMap((n) => [`guide-step${n}-title`, `guide-step${n}-body`]),
      ...["burn-in-keep-originals", "burn-in-only", "separate-file", "ignore"].flatMap((m) => [`overlay-${m}`, `overlay-${m}-desc`]),
      ...["done", "partial", "failed", "cancelled"].map((s) => `results-announce-${s}`),
    ];
    for (const id of dynamic) if (!ids.has(id)) missing.push(id);
    expect(missing).toEqual([]);
  });

  it("has plain-language copy and a next step for every SRS §8 error code", () => {
    const i18n = createI18n("en-US", ["en-US"]);
    for (const code of ERROR_CODES) {
      const id = errorMessageId(code);
      expect(i18n.has(id), id).toBe(true);
      expect(i18n.attr(id, "next")).not.toBe(id);
      expect(i18n.t(id)).not.toMatch(/[A-Z]{2,}_[A-Z]/);
    }
  });

  it("selects plurals and formats numbers in the user's locale", () => {
    const us = createI18n("system", ["en-US"]);
    expect(us.t("inputs-added", { count: 1 })).toBe("Added one file.");
    expect(us.t("inputs-added", { count: 4812 })).toBe("Added 4,812 files.");
    expect(us.t("inputs-added", { count: 0 })).toBe("No new files added.");
    const de = createI18n("system", ["de-DE"]);
    expect(de.language).toBe("en-US");
    expect(de.formatLocale).toBe("de-DE");
    expect(de.t("inputs-added", { count: 4812 })).toBe("Added 4.812 files.");
    expect(de.t("inputs-added", { count: 1 })).toBe("Added one file.");
  });

  it("formats rates with the given fraction digits and still picks the plural", () => {
    const i18n = createI18n("en-US", ["en-US"]);
    expect(i18n.t("processing-rate", { rate: i18n.num(2.4, { maximumFractionDigits: 1, minimumFractionDigits: 1 }) })).toBe("2.4 items per second");
    expect(i18n.t("processing-rate", { rate: i18n.num(1) })).toBe("1 item per second");
  });
});

describe("negotiation and markup", () => {
  it("falls back to en-US and keeps the user's formatting locale", () => {
    expect(negotiate("system", ["pt-BR", "en-GB"])).toEqual({ language: "en-US", formatLocale: "pt-BR" });
    expect(negotiate("en-US", ["fr-FR"])).toEqual({ language: "en-US", formatLocale: "en-US" });
    expect(negotiate("system", ["not a locale"])).toEqual({ language: "en-US", formatLocale: "en-US" });
  });

  it("splits simple tags", () => {
    expect(parseRich("Go to <b>My Data</b> now")).toEqual([{ text: "Go to " }, { tag: "b", text: "My Data" }, { text: " now" }]);
  });
});

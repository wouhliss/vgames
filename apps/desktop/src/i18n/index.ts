// Tiny typed i18n layer. Every user-visible string goes through `t()`.
//
// - Keys are dotted paths into the English catalog (`t("common.cancel")`); a typo is a type error.
// - `{name}` placeholders are extracted from the message type, so missing or unknown params are
//   type errors too.
// - Plural messages are objects with CLDR categories (`one`, `other`, …) and take a numeric `count`.
//   So never name an ordinary catalog key `other`: its group would be read as a plural message.
//
// There is only an English catalog today. Other locales implement the `Catalog` shape and are
// installed with `setCatalog`.
import { en } from "./en";

type PluralMessage = { readonly other: string } & Partial<
  Record<"zero" | "one" | "two" | "few" | "many", string>
>;

type Leaves<T, P extends string = ""> = {
  [K in keyof T & string]: T[K] extends string
    ? `${P}${K}`
    : T[K] extends PluralMessage
      ? `${P}${K}`
      : Leaves<T[K], `${P}${K}.`>;
}[keyof T & string];

type At<T, K extends string> = K extends `${infer H}.${infer R}`
  ? H extends keyof T
    ? At<T[H], R>
    : never
  : K extends keyof T
    ? T[K]
    : never;

type Placeholders<S> = S extends `${string}{${infer P}}${infer Rest}`
  ? P | Placeholders<Rest>
  : never;

type MessageParams<M> = M extends string
  ? Placeholders<M>
  : M extends PluralMessage
    ? Placeholders<M[keyof M]> | "count"
    : never;

export type MessageKey = Leaves<typeof en>;
type ParamValue = string | number;
type ParamsFor<K extends MessageKey> = [MessageParams<At<typeof en, K>>] extends [never]
  ? []
  : [params: { [P in MessageParams<At<typeof en, K>>]: P extends "count" ? number : ParamValue }];

type DeepCatalog<T> = {
  [K in keyof T]: T[K] extends string
    ? string
    : T[K] extends PluralMessage
      ? PluralMessage
      : DeepCatalog<T[K]>;
};
export type Catalog = DeepCatalog<typeof en>;

let catalog: Catalog = en;
let locale = "en";
let pluralRules = new Intl.PluralRules(locale);

export function setCatalog(nextLocale: string, next: Catalog): void {
  catalog = next;
  locale = nextLocale;
  pluralRules = new Intl.PluralRules(nextLocale);
}

export function currentLocale(): string {
  return locale;
}

function lookup(key: string): string | PluralMessage | undefined {
  let node: unknown = catalog;
  for (const part of key.split(".")) {
    if (node === null || typeof node !== "object") return undefined;
    node = (node as Record<string, unknown>)[part];
  }
  if (typeof node === "string") return node;
  if (node !== null && typeof node === "object" && "other" in node) return node as PluralMessage;
  return undefined;
}

function interpolate(message: string, params: Record<string, ParamValue> | undefined): string {
  if (!params) return message;
  return message.replace(/\{(\w+)\}/g, (whole, name: string) => {
    const value = params[name];
    if (value === undefined) return whole;
    return typeof value === "number" ? formatNumber(value) : value;
  });
}

export function t<K extends MessageKey>(key: K, ...args: ParamsFor<K>): string {
  const params = args[0] as Record<string, ParamValue> | undefined;
  const message = lookup(key);
  if (message === undefined) {
    // Visible in development, harmless in production: the key itself is shown.
    if (import.meta.env.DEV) console.warn(`i18n: missing message "${key}"`);
    return key;
  }
  if (typeof message === "string") return interpolate(message, params);
  const count = typeof params?.count === "number" ? params.count : 0;
  const category = pluralRules.select(count) as keyof PluralMessage;
  return interpolate(message[category] ?? message.other, params);
}

// ---------------------------------------------------------------------------------------------
// Formatting helpers (locale-aware, never hand-rolled number formats in components).

const numberFormats = new Map<string, Intl.NumberFormat>();
function numberFormat(options: Intl.NumberFormatOptions): Intl.NumberFormat {
  const cacheKey = `${locale}|${JSON.stringify(options)}`;
  let format = numberFormats.get(cacheKey);
  if (!format) {
    format = new Intl.NumberFormat(locale, options);
    numberFormats.set(cacheKey, format);
  }
  return format;
}

export function formatNumber(value: number): string {
  return numberFormat({}).format(value);
}

export function formatPercent(fraction: number): string {
  return numberFormat({ style: "percent", maximumFractionDigits: 0 }).format(fraction);
}

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;

/** Binary sizes shown with familiar unit names (1 GB = 1024³ bytes, as file managers show them). */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1;
  return `${numberFormat({ maximumFractionDigits: digits, minimumFractionDigits: digits }).format(value)} ${BYTE_UNITS[unit]}`;
}

export function formatRate(bytesPerSecond: number): string {
  return t("format.rate", { size: formatBytes(bytesPerSecond) });
}

/** Durations such as an ETA: "45 s", "3 min", "1 h 20 min". */
export function formatDuration(totalSeconds: number): string {
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return "—";
  const s = Math.round(totalSeconds);
  if (s < 60) return t("format.seconds", { count: s });
  const minutes = Math.round(s / 60);
  if (minutes < 60) return t("format.minutes", { count: minutes });
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0
    ? t("format.hours", { count: hours })
    : t("format.hoursMinutes", { hours, minutes: rest });
}

const relativeFormats = new Map<string, Intl.RelativeTimeFormat>();
export function formatRelativeTime(iso: string, now: Date = new Date()): string {
  const then = new Date(iso);
  if (Number.isNaN(then.getTime())) return "—";
  let format = relativeFormats.get(locale);
  if (!format) {
    format = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
    relativeFormats.set(locale, format);
  }
  const seconds = Math.round((then.getTime() - now.getTime()) / 1000);
  const abs = Math.abs(seconds);
  if (abs < 60) return format.format(seconds, "second");
  if (abs < 3600) return format.format(Math.round(seconds / 60), "minute");
  if (abs < 86400) return format.format(Math.round(seconds / 3600), "hour");
  if (abs < 86400 * 30) return format.format(Math.round(seconds / 86400), "day");
  if (abs < 86400 * 365) return format.format(Math.round(seconds / (86400 * 30)), "month");
  return format.format(Math.round(seconds / (86400 * 365)), "year");
}

export function formatDateTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "short" }).format(date);
}

export function formatDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "—";
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(date);
}

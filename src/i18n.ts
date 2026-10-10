import ar from "./locales/ar";
import de from "./locales/de";
import en from "./locales/en";
import es from "./locales/es";
import fr from "./locales/fr";
import hi from "./locales/hi";
import it from "./locales/it";
import ja from "./locales/ja";
import ko from "./locales/ko";
import pt from "./locales/pt";
import ru from "./locales/ru";
import zh from "./locales/zh";

export type Lang = "en" | "ru" | "es" | "pt" | "fr" | "de" | "it" | "zh" | "ja" | "ko" | "hi" | "ar";

export const LANGS: { code: Lang; name: string; rtl?: boolean }[] = [
  { code: "en", name: "English" },
  { code: "ru", name: "Русский" },
  { code: "es", name: "Español" },
  { code: "pt", name: "Português" },
  { code: "fr", name: "Français" },
  { code: "de", name: "Deutsch" },
  { code: "it", name: "Italiano" },
  { code: "zh", name: "中文" },
  { code: "ja", name: "日本語" },
  { code: "ko", name: "한국어" },
  { code: "hi", name: "हिन्दी" },
  { code: "ar", name: "العربية", rtl: true },
];

export type Dict = {
  statusOn: string;
  statusOff: string;
  nextAt: string;
  pressToStart: string;
  noPrinter: string;
  printer: string;
  online: string;
  offline: string;
  localPrinter: string;
  frequency: string;
  days: (n: number) => string;
  at: string;
  whatToPrint: string;
  modeNozzle: string;
  modeNozzleDesc: string;
  modePage: string;
  modePageDesc: string;
  printNow: string;
  printing: string;
  clean: string;
  cleanConfirm: string;
  settings: string;
  back: string;
  smartSkip: string;
  smartSkipDesc: string;
  onTop: string;
  onTopDesc: string;
  autostart: string;
  autostartDesc: string;
  autostartBlocked: string;
  autostartDev: string;
  startMinimized: string;
  startMinimizedDesc: string;
  language: string;
  history: string;
  historyEmpty: string;
  clearHistory: string;
  kind: Record<string, string>;
  auto: string;
  manual: string;
  failed: string;
  ready: string;
  off: string;
  inQueue: string;
  nextIn: string;
  last: string;
  never: string;
  unit: { d: string; h: string; m: string };
  note: string;
  // tray + notifications (sent to Rust)
  trayStatusOn: string;
  trayStatusOff: string;
  trayEnable: string;
  trayDisable: string;
  trayShow: string;
  trayQuit: string;
  tooltipOn: string;
  tooltipOff: string;
  nDoneTitle: string;
  nDoneBody: string;
  nFailTitle: string;
  nOfflineTitle: string;
  nOfflineBody: string;
  pageTitle: string;
  pageSubtitle: string;
  nStuckTitle: string;
  nStuckBody: string;
  nInkTitle: string;
  nWasteTitle: string;
  nWasteBody: string;
  // consumables
  inks: string;
  inkNames: Record<string, string>;
  inkShort: Record<string, string>;
  inkNoData: string;
  inkEstimate: string;
  pages: string;
  waste: string;
  wasteState: Record<string, string>;
  updated: string;
  refresh: string;
  // queue
  stuck: string;
  clearQueue: string;
  refreshStatus: string;
  choosePrinter: string;
  defaultPrinter: string;
  noPrinters: string;
  hours: string;
  minutes: string;
  done: string;
  vpnBlocks: string;
  vpnHint: (name: string) => string;
  // quiet time
  quietHours: string;
  quietHoursDesc: string;
  quietDays: string;
  quietDaysDesc: string;
  from: string;
  to: string;
  weekdays: string[];
  // updates
  updCheck: string;
  updChecking: string;
  updUpToDate: string;
  updAvailable: string;
  updInstall: string;
  updDownloading: string;
  updError: string;
};

export const translations: Record<Lang, Dict> = { en, ru, es, pt, fr, de, it, zh, ja, ko, hi, ar };

/** Pick the best supported language from the system locale, falling back to English. */
export function detectLang(): Lang {
  const sys = (typeof navigator !== "undefined" ? navigator.language : "en") || "en";
  const code = sys.toLowerCase().split("-")[0];
  // Neighbouring languages without their own translation read Russian best.
  if (["uk", "be", "kk"].includes(code)) return "ru";
  return code in translations ? (code as Lang) : "en";
}

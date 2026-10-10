<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { LogicalSize } from "@tauri-apps/api/dpi";
  import { load } from "@tauri-apps/plugin-store";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { getVersion } from "@tauri-apps/api/app";
  import { check } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";
  import { onMount, tick } from "svelte";
  import { fly, slide } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { LANGS, translations, detectLang, type Lang } from "./i18n";
  import TimePanel from "./TimePanel.svelte";

  type Mode = "nozzle" | "page";
  type HistoryEntry = { ts: number; kind: string; auto: boolean; ok: boolean; detail: string };
  type Ink = { key: string; label: string; level: number | null };
  type Supplies = {
    ts: number;
    inks: Ink[];
    wastePct: number | null;
    wasteState: string | null;
    pages: number | null;
  };
  type Settings = {
    enabled: boolean;
    printer: string | null;
    mode: Mode;
    intervalDays: number;
    timeMinute: number;
    smartSkip: boolean;
    startMinimized: boolean;
    quietDays: number;
    quietHours: boolean;
    quietFrom: number;
    quietTo: number;
    lastRun: number | null;
    lastActivity: number | null;
    history: HistoryEntry[];
    supplies: Supplies | null;
  };
  type Status = {
    settings: Settings;
    nextRun: number | null;
    online: boolean | null;
    jobs: number;
    busy: boolean;
    stuck: boolean;
    vpn: string | null;
    now: number;
  };
  type PrinterInfo = { name: string; port: string; isDefault: boolean; host: string | null };

  let st = $state<Status | null>(null);
  let printers = $state<PrinterInfo[]>([]);
  let onTop = $state(false);
  type Autostart = { on: boolean; blocked: boolean; dev: boolean };
  let autostart = $state<Autostart>({ on: false, blocked: false, dev: false });
  type View = "main" | "settings" | "history" | "language";
  let view = $state<View>("main");
  let lang = $state<Lang>("ru");
  let appVersion = $state("");
  let cleanArmed = $state(false);
  let actionError = $state("");
  let updateState = $state<
    "idle" | "checking" | "available" | "downloading" | "uptodate" | "error"
  >("idle");
  let updateVersion = $state("");
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let pendingUpdate: any = null;

  const t = $derived(translations[lang]);
  const s = $derived(st?.settings);

  const isMac =
    typeof navigator !== "undefined" && /Mac OS X|Macintosh/.test(navigator.userAgent);
  if (isMac && typeof document !== "undefined") {
    document.documentElement.classList.add("mac");
  }

  const intervalPresets = [1, 3, 5, 7];

  const appWin = getCurrentWindow();

  // Durations: "2 д 4 ч" for days, "4 ч 12 мин" for hours, "12:05" under an hour.
  function fmtDur(sec: number) {
    sec = Math.max(0, Math.floor(sec));
    const d = Math.floor(sec / 86400);
    const h = Math.floor((sec % 86400) / 3600);
    const m = Math.floor((sec % 3600) / 60);
    if (d > 0) return `${d} ${t.unit.d} ${h} ${t.unit.h}`;
    if (h > 0) return `${h} ${t.unit.h} ${m} ${t.unit.m}`;
    return `${m}:${String(sec % 60).padStart(2, "0")}`;
  }
  function fmtDate(ts: number) {
    return new Intl.DateTimeFormat(lang, {
      weekday: "short",
      day: "numeric",
      month: "short",
      hour: "2-digit",
      minute: "2-digit",
    }).format(new Date(ts * 1000));
  }
  const hhmm = (min: number) =>
    `${String(Math.floor(min / 60)).padStart(2, "0")}:${String(min % 60).padStart(2, "0")}`;

  // Which inline picker is expanded: the printer list or one of the time fields.
  type Picker = "printer" | "timeMinute" | "quietFrom" | "quietTo";
  let openPicker = $state<Picker | null>(null);
  async function togglePicker(p: Picker) {
    if (openPicker === p) {
      openPicker = null;
      return;
    }
    await reserveHeight(p === "printer" ? 70 * printers.length + 20 : 80);
    openPicker = p;
  }
  // Steps from the time panel are saved after a short pause; "Done" saves now.
  let timeSave: ReturnType<typeof setTimeout> | null = null;
  function pickTime(key: "timeMinute" | "quietFrom" | "quietTo") {
    return (value: number, done: boolean) => {
      if (timeSave) clearTimeout(timeSave);
      if (done) {
        patch({ [key]: value });
        openPicker = null;
      } else {
        timeSave = setTimeout(() => patch({ [key]: value }), 300);
      }
    };
  }
  function pickPrinter(name: string) {
    openPicker = null;
    if (name !== s?.printer) patch({ printer: name });
  }

  const inkColors: Record<string, string> = {
    black: "#1c1c1e",
    photoBlack: "#3a3a3c",
    matteBlack: "#2c2c2e",
    cyan: "#00aeef",
    magenta: "#ec008c",
    yellow: "#ffd60a",
    grey: "#8e8e93",
    lightCyan: "#7fd6f7",
    lightMagenta: "#f57fc5",
    red: "#ff3b30",
  };
  const inkName = (i: Ink) => t.inkNames[i.key] ?? i.label;
  const sup = $derived(s?.supplies ?? null);
  const hasLevels = $derived(!!sup && sup.inks.some((i) => i.level !== null));

  const depth = { main: 0, settings: 1, history: 2, language: 2 };
  let navDir = $state(1);
  function go(target: View) {
    navDir = depth[target] >= depth[view] ? 1 : -1;
    openPicker = null;
    view = target;
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let store: any = null;

  async function patch(p: Partial<Settings>) {
    st = await invoke<Status>("patch_settings", { patch: p });
  }

  function trayLabels() {
    return {
      status_on: t.trayStatusOn,
      status_off: t.trayStatusOff,
      enable: t.trayEnable,
      disable: t.trayDisable,
      print_now: t.printNow,
      show: t.trayShow,
      quit: t.trayQuit,
      tooltip_on: t.tooltipOn,
      tooltip_off: t.tooltipOff,
      done_title: t.nDoneTitle,
      done_body: t.nDoneBody,
      fail_title: t.nFailTitle,
      offline_title: t.nOfflineTitle,
      offline_body: t.nOfflineBody,
      page_title: t.pageTitle,
      page_subtitle: t.pageSubtitle,
      stuck_title: t.nStuckTitle,
      stuck_body: t.nStuckBody,
      ink_title: t.nInkTitle,
      waste_title: t.nWasteTitle,
      waste_body: t.nWasteBody,
      inks: t.inkNames,
    };
  }

  $effect(() => {
    document.documentElement.lang = lang;
    document.documentElement.dir = LANGS.find((l) => l.code === lang)?.rtl ? "rtl" : "ltr";
    store?.set("lang", lang);
    invoke("set_labels", { labels: trayLabels() }).catch(() => {});
  });

  $effect(() => {
    view;
    requestAnimationFrame(fitWindow);
  });

  // The OS window follows the card's height. Growing happens at once, but
  // shrinking waits until animations settle: the window is transparent, so
  // being briefly taller is invisible, while being shorter clips the card.
  let winHeight = 0;
  let shrinkTimer: ReturnType<typeof setTimeout> | null = null;
  async function setWinHeight(h: number) {
    if (h === winHeight) return;
    winHeight = h;
    await appWin.setSize(new LogicalSize(420, h));
  }
  async function fitWindow() {
    await tick();
    const el = document.querySelector(".window") as HTMLElement | null;
    if (!el) return;
    const h = Math.ceil(el.getBoundingClientRect().height);
    if (shrinkTimer) clearTimeout(shrinkTimer);
    shrinkTimer = null;
    if (h >= winHeight) await setWinHeight(h);
    else shrinkTimer = setTimeout(() => setWinHeight(h), 260);
  }
  /** Make room before something expands, so the animation never gets clipped. */
  async function reserveHeight(extra: number) {
    const el = document.querySelector(".window") as HTMLElement | null;
    if (!el) return;
    if (shrinkTimer) clearTimeout(shrinkTimer);
    shrinkTimer = null;
    await setWinHeight(Math.max(winHeight, Math.ceil(el.getBoundingClientRect().height) + extra));
    // Hand control back to fitWindow once the expansion has finished.
    shrinkTimer = setTimeout(fitWindow, 400);
  }

  async function refreshPrinters() {
    try {
      printers = await invoke<PrinterInfo[]>("list_printers");
    } catch (e) {
      console.error("list printers failed", e);
    }
  }

  onMount(async () => {
    st = await invoke<Status>("get_status");
    await listen<Status>("status", (e) => (st = e.payload));
    refreshPrinters();
    loadAutostart();
    getVersion()
      .then((v) => (appVersion = v))
      .catch(() => {});

    try {
      store = await load("ui.json", { autoSave: 200 });
      const saved = (await store.get("lang")) as Lang | null;
      lang = saved && translations[saved] ? saved : detectLang();
    } catch (_) {
      lang = detectLang();
    }

    const el = document.querySelector(".window");
    if (el && "ResizeObserver" in window) {
      new ResizeObserver(() => fitWindow()).observe(el);
    }
    requestAnimationFrame(fitWindow);

    suppressHoverUntilMove();
    appWin.onFocusChanged(({ payload: focused }) => {
      if (focused) {
        suppressHoverUntilMove();
        refreshPrinters();
        loadAutostart();
      }
    });

    // Quietly check for updates on launch.
    checkUpdate(false);
  });

  async function checkUpdate(manual = false) {
    if (updateState === "checking" || updateState === "downloading") return;
    updateState = "checking";
    try {
      const upd = await check();
      if (upd) {
        pendingUpdate = upd;
        updateVersion = upd.version;
        updateState = "available";
      } else {
        updateState = manual ? "uptodate" : "idle";
      }
    } catch (e) {
      console.error("update check failed", e);
      updateState = manual ? "error" : "idle";
    }
  }
  async function installUpdate() {
    if (!pendingUpdate) return;
    updateState = "downloading";
    try {
      await pendingUpdate.downloadAndInstall();
      await relaunch();
    } catch (e) {
      console.error("update install failed", e);
      updateState = "error";
    }
  }
  function onUpdateRow() {
    if (updateState === "available") installUpdate();
    else checkUpdate(true);
  }

  let refreshing = $state(false);
  async function refreshStatus() {
    if (refreshing) return;
    refreshing = true;
    try {
      await refreshPrinters();
      st = await invoke<Status>("refresh_status");
    } catch (e) {
      console.error("refresh failed", e);
    } finally {
      refreshing = false;
    }
  }

  async function clearQueue() {
    actionError = "";
    try {
      st = await invoke<Status>("clear_queue");
    } catch (e) {
      actionError = String(e);
    }
  }

  function toggleQuietDay(i: number) {
    if (!s) return;
    patch({ quietDays: s.quietDays ^ (1 << i) });
  }

  async function toggleEnabled() {
    if (!s) return;
    if (!s.printer) return;
    await patch({ enabled: !s.enabled });
  }

  async function printNow() {
    actionError = "";
    try {
      await invoke("print_now");
    } catch (e) {
      actionError = String(e);
    }
  }

  async function onClean() {
    if (!cleanArmed) {
      cleanArmed = true;
      setTimeout(() => (cleanArmed = false), 4000);
      return;
    }
    cleanArmed = false;
    actionError = "";
    try {
      await invoke("run_action", { action: "clean" });
    } catch (e) {
      actionError = String(e);
    }
  }

  async function toggleOnTop() {
    onTop = !onTop;
    await appWin.setAlwaysOnTop(onTop);
  }
  async function loadAutostart() {
    try {
      autostart = await invoke<Autostart>("get_autostart");
    } catch (e) {
      console.error("autostart state failed", e);
    }
  }
  async function toggleAutostart() {
    try {
      // While Windows blocks it, the switch shows off; turning it on re-allows it.
      const on = !(autostart.on && !autostart.blocked);
      autostart = await invoke<Autostart>("set_autostart", { on });
    } catch (e) {
      console.error("autostart toggle failed", e);
    }
  }



  const selectedPrinter = $derived(printers.find((p) => p.name === s?.printer));

  function suppressHoverUntilMove() {
    document.documentElement.classList.add("suppress-hover");
    (document.activeElement as HTMLElement | null)?.blur();
    const clear = () => {
      document.documentElement.classList.remove("suppress-hover");
      window.removeEventListener("mousemove", clear);
    };
    window.addEventListener("mousemove", clear);
  }
</script>

<main class="window">
  <div class="titlebar" data-tauri-drag-region>
    <button
      class="icon-btn gear {isMac ? 'mac' : 'win'}"
      title={view === "main" ? t.settings : t.back}
      aria-label={view === "main" ? t.settings : t.back}
      onclick={() => go(view === "main" ? "settings" : view === "settings" ? "main" : "settings")}
    >
      {#if view !== "main"}
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M19 12H5" />
          <path d="M12 19l-7-7 7-7" />
        </svg>
      {:else}
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.830l-.06-.06a1.65 1.65 0 0 0-1.820-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
      {/if}
    </button>

    <div class="title">
      {view === "main" ? "Print Pulse" : view === "history" ? t.history : view === "language" ? t.language : t.settings}
    </div>

    {#if !isMac}
      <div class="win-controls">
        <button class="win-btn" aria-label="minimize" onclick={() => appWin.minimize()}>
          <svg width="11" height="11" viewBox="0 0 11 11"><rect x="1" y="5" width="9" height="1" fill="currentColor" /></svg>
        </button>
        <button class="win-btn close" aria-label="close" onclick={() => appWin.hide()}>
          <svg width="11" height="11" viewBox="0 0 11 11"><path d="M1 1 L10 10 M10 1 L1 10" stroke="currentColor" stroke-width="1.1" /></svg>
        </button>
      </div>
    {/if}
  </div>

  <div class="content">
    {#if s && st}
      {#key view}
        <div class="view-wrap" in:fly={{ x: navDir * 24, duration: 200, opacity: 0, easing: cubicOut }}>
          {#if view === "main"}
            <!-- Power + printer -->
            <div class="card group">
              <button class="row" onclick={toggleEnabled} disabled={!s.printer}>
                <div class="power-left">
                  <span class="dot {s.enabled ? 'live' : ''}"></span>
                  <div class="labels">
                    <span class="primary">{s.enabled ? t.statusOn : t.statusOff}</span>
                    <span class="secondary">
                      {#if !s.printer}{t.noPrinter}
                      {:else if s.enabled && st.nextRun}{t.nextAt}: {fmtDate(st.nextRun)}
                      {:else}{t.pressToStart}{/if}
                    </span>
                  </div>
                </div>
                <span class="switch {s.enabled ? 'checked' : ''}"><span class="knob"></span></span>
              </button>
              <div class="row sub static">
                <div class="labels">
                  <span class="primary">{t.printer}</span>
                  <span class="secondary net">
                    {#if !selectedPrinter?.host}{t.localPrinter}
                    {:else}
                      <span class="mini-dot {st.online === true ? 'ok' : st.online === false ? 'bad' : ''}"></span>
                      {selectedPrinter.host} · {st.online === false ? (st.vpn ? t.vpnBlocks : t.offline) : t.online}
                    {/if}
                    <button
                      class="refresh {refreshing ? 'spin' : ''}"
                      title={t.refreshStatus}
                      aria-label={t.refreshStatus}
                      onclick={refreshStatus}
                      disabled={refreshing || !s.printer}
                    >
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M21 12a9 9 0 1 1-2.64-6.36" />
                        <path d="M21 3v6h-6" />
                      </svg>
                    </button>
                  </span>
                </div>
                <button
                  class="picker {openPicker === 'printer' ? 'open' : ''}"
                  onclick={() => togglePicker("printer")}
                  title={s.printer ?? ""}
                >
                  <span class="picker-text">{s.printer ?? t.choosePrinter}</span>
                  <svg class="chev" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M6 9l6 6 6-6" />
                  </svg>
                </button>
              </div>
              {#if openPicker === "printer"}
                <div class="plist" transition:slide={{ duration: 200, easing: cubicOut }}>
                  {#each printers as p}
                    <button class="row sub popt {p.name === s.printer ? 'selected' : ''}" onclick={() => pickPrinter(p.name)}>
                      <div class="labels">
                        <span class="primary">{p.name}</span>
                        <span class="secondary">{p.host ?? p.port}{p.isDefault ? ` · ${t.defaultPrinter}` : ""}</span>
                      </div>
                      {#if p.name === s.printer}
                        <svg class="check" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round">
                          <path d="M5 12l5 5 9-10" />
                        </svg>
                      {/if}
                    </button>
                  {:else}
                    <div class="row sub static"><span class="secondary">{t.noPrinters}</span></div>
                  {/each}
                </div>
              {/if}
              {#if st.online === false && st.vpn}
                <div class="row sub static"><span class="secondary warn-text">{t.vpnHint(st.vpn)}</span></div>
              {/if}
            </div>

            <!-- Frequency + time -->
            <div class="section-label">{t.frequency}</div>
            <div class="freq">
              <div class="presets five">
                {#each intervalPresets as v}
                  <button class="preset {s.intervalDays === v ? 'active' : ''}" onclick={() => patch({ intervalDays: v })}>
                    {t.days(v)}
                  </button>
                {/each}
                <button
                  class="preset time {openPicker === 'timeMinute' ? 'active' : ''}"
                  title={t.at}
                  onclick={() => togglePicker("timeMinute")}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                    <circle cx="12" cy="12" r="9" />
                    <path d="M12 7v5l3 2" />
                  </svg>
                  {hhmm(s.timeMinute)}
                </button>
              </div>
              {#if openPicker === "timeMinute"}
                <div class="card static picker-card" transition:slide={{ duration: 200, easing: cubicOut }}>
                  <TimePanel value={s.timeMinute} hoursLabel={t.hours} minutesLabel={t.minutes} doneLabel={t.done} onpick={pickTime("timeMinute")} />
                </div>
              {/if}
            </div>

            <!-- Mode -->
            <div class="section-label">{t.whatToPrint}</div>
            <div class="segs">
              <button class="seg {s.mode === 'nozzle' ? 'active' : ''}" onclick={() => patch({ mode: "nozzle" })}>
                <span class="primary">{t.modeNozzle}</span>
                <span class="secondary">{t.modeNozzleDesc}</span>
              </button>
              <button class="seg {s.mode === 'page' ? 'active' : ''}" onclick={() => patch({ mode: "page" })}>
                <span class="primary">{t.modePage}</span>
                <span class="secondary">{t.modePageDesc}</span>
              </button>
            </div>

            <div class="actions">
              <button class="btn accent" onclick={printNow} disabled={st.busy || !s.printer}>
                {st.busy ? t.printing : t.printNow}
              </button>
              <button class="btn {cleanArmed ? 'warn' : ''}" onclick={onClean} disabled={st.busy || !s.printer}>
                {cleanArmed ? t.cleanConfirm : t.clean}
              </button>
            </div>
            {#if actionError}<div class="error">{actionError}</div>{/if}

            <!-- Consumables (last known, also while the printer is off) -->
            {#if sup}
              <div class="card group">
                <div class="row static ink-head">
                  <span class="primary">{t.inks}</span>
                  <span class="secondary">
                    {t.updated} {fmtDate(sup.ts)} ·
                    <button class="link small" onclick={() => invoke("refresh_supplies")} disabled={st.online !== true}>{t.refresh}</button>
                  </span>
                </div>
                {#if hasLevels}
                  <div class="tanks">
                    {#each sup.inks as ink}
                      <div class="tank" title={inkName(ink)}>
                        <div class="tank-body {ink.level !== null && ink.level <= 15 ? 'low' : ''}">
                          <div
                            class="tank-fill"
                            style="height: {ink.level ?? 0}%; background: {inkColors[ink.key] ?? 'var(--text-dim)'}"
                          ></div>
                        </div>
                        <span class="tank-name">{t.inkShort[ink.key] ?? ink.label.slice(0, 3)}</span>
                        <span class="tank-pct">{ink.level === null ? "—" : `${ink.level}%`}</span>
                      </div>
                    {/each}
                  </div>
                {:else}
                  <div class="row static sub"><span class="secondary">{t.inkNoData}</span></div>
                {/if}
                {#if sup.pages !== null || sup.wastePct !== null || sup.wasteState}
                  <div class="row static sub meta">
                    {#if sup.pages !== null}
                      <span class="secondary">{t.pages}: <b>{sup.pages.toLocaleString(lang)}</b></span>
                    {/if}
                    {#if sup.wastePct !== null || sup.wasteState}
                      <span class="secondary">
                        {t.waste}:
                        <b class={(sup.wastePct ?? 0) >= 90 || sup.wasteState === "near" || sup.wasteState === "full" ? "warn-text" : ""}>
                          {sup.wastePct !== null ? `${sup.wastePct}%` : (t.wasteState[sup.wasteState ?? ""] ?? sup.wasteState)}
                        </b>
                      </span>
                    {/if}
                  </div>
                {/if}
              </div>
            {/if}
          {:else if view === "settings"}
            <button class="card slim" onclick={() => patch({ smartSkip: !s.smartSkip })}>
              <div class="labels">
                <span class="primary">{t.smartSkip}</span>
                <span class="secondary">{t.smartSkipDesc}</span>
              </div>
              <span class="switch {s.smartSkip ? 'checked' : ''}"><span class="knob"></span></span>
            </button>

            <div class="card group">
              <button class="row" onclick={() => patch({ quietHours: !s.quietHours })}>
                <div class="labels">
                  <span class="primary">{t.quietHours}</span>
                  <span class="secondary">{t.quietHoursDesc}</span>
                </div>
                <span class="switch {s.quietHours ? 'checked' : ''}"><span class="knob"></span></span>
              </button>
              {#if s.quietHours}
                <div class="row sub static quiet-range">
                  <span class="secondary">{t.from}</span>
                  <button class="time-chip {openPicker === 'quietFrom' ? 'open' : ''}" onclick={() => togglePicker("quietFrom")}>
                    {hhmm(s.quietFrom)}
                  </button>
                  <span class="secondary">{t.to}</span>
                  <button class="time-chip {openPicker === 'quietTo' ? 'open' : ''}" onclick={() => togglePicker("quietTo")}>
                    {hhmm(s.quietTo)}
                  </button>
                </div>
                {#if openPicker === "quietFrom" || openPicker === "quietTo"}
                  <div class="row sub static picker-row" transition:slide={{ duration: 200, easing: cubicOut }}>
                    {#key openPicker}
                      <TimePanel
                        value={openPicker === "quietFrom" ? s.quietFrom : s.quietTo}
                        hoursLabel={t.hours}
                        minutesLabel={t.minutes}
                        doneLabel={t.done}
                        onpick={pickTime(openPicker)}
                      />
                    {/key}
                  </div>
                {/if}
              {/if}
              <div class="row sub static column">
                <div class="labels">
                  <span class="primary">{t.quietDays}</span>
                  <span class="secondary">{t.quietDaysDesc}</span>
                </div>
                <div class="weekdays">
                  {#each t.weekdays as d, i}
                    <button class="chip {s.quietDays & (1 << i) ? 'active' : ''}" onclick={() => toggleQuietDay(i)}>{d}</button>
                  {/each}
                </div>
              </div>
            </div>

            <button class="card slim" onclick={toggleOnTop}>
              <div class="labels">
                <span class="primary">{t.onTop}</span>
                <span class="secondary">{t.onTopDesc}</span>
              </div>
              <span class="switch {onTop ? 'checked' : ''}"><span class="knob"></span></span>
            </button>

            <div class="card group">
              <button class="row" onclick={toggleAutostart}>
                <div class="labels">
                  <span class="primary">{t.autostart}</span>
                  <span class="secondary {autostart.blocked ? 'warn-text' : ''}">
                    {autostart.blocked ? t.autostartBlocked : autostart.dev ? t.autostartDev : t.autostartDesc}
                  </span>
                </div>
                <span class="switch {autostart.on && !autostart.blocked ? 'checked' : ''}"><span class="knob"></span></span>
              </button>
              {#if autostart.on}
                <button class="row sub" onclick={() => patch({ startMinimized: !s.startMinimized })}>
                  <div class="labels">
                    <span class="primary">{t.startMinimized}</span>
                    <span class="secondary">{t.startMinimizedDesc}</span>
                  </div>
                  <span class="switch {s.startMinimized ? 'checked' : ''}"><span class="knob"></span></span>
                </button>
              {/if}
            </div>

            <button class="card slim" onclick={() => go("language")}>
              <span class="primary">{t.language}</span>
              <span class="lang-current">
                {LANGS.find((l) => l.code === lang)?.name}
                <svg class="chev-right" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M9 6l6 6-6 6" />
                </svg>
              </span>
            </button>

            <button class="card slim" onclick={() => go("history")}>
              <span class="primary">{t.history}</span>
              <span class="lang-current">
                {s.history.length}
                <svg class="chev-right" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M9 6l6 6-6 6" />
                </svg>
              </span>
            </button>

            <button
              class="card slim"
              onclick={onUpdateRow}
              disabled={updateState === "checking" || updateState === "downloading"}
            >
              <span class="primary">
                {updateState === "available"
                  ? t.updAvailable
                  : updateState === "uptodate"
                    ? t.updUpToDate
                    : updateState === "error"
                      ? t.updError
                      : t.updCheck}
              </span>
              <span class="update-action {updateState === 'available' ? 'accent' : ''}">
                {#if updateState === "checking"}{t.updChecking}
                {:else if updateState === "downloading"}{t.updDownloading}
                {:else if updateState === "available"}v{updateVersion} · {t.updInstall}{/if}
              </span>
            </button>

            <div class="note">{t.note}</div>

            <div class="footer">
              Print Pulse{appVersion ? ` · v${appVersion}` : ""} · <button class="link" onclick={() => openUrl("https://drxid.ru/")}>drxid</button>
            </div>
          {:else if view === "language"}
            <!-- Language selection (scrolls; never stretches the window) -->
            <div class="lang-list-full">
              {#each LANGS as l}
                <button
                  class="lang-item {l.code === lang ? 'active' : ''}"
                  onclick={() => {
                    lang = l.code;
                    go("settings");
                  }}
                >
                  {l.name}
                  {#if l.code === lang}
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                      <path d="M20 6L9 17l-5-5" />
                    </svg>
                  {/if}
                </button>
              {/each}
            </div>
          {:else}
            {#if s.history.length === 0}
              <div class="note empty">{t.historyEmpty}</div>
            {:else}
              <div class="history">
                {#each s.history as h}
                  <div class="h-item">
                    <span class="mini-dot {h.ok ? 'ok' : 'bad'}"></span>
                    <div class="labels">
                      <span class="primary">{t.kind[h.kind] ?? h.kind}</span>
                      <span class="secondary">
                        {fmtDate(h.ts)} · {h.auto ? t.auto : t.manual}{#if !h.ok && h.kind !== "offline"} · {t.failed}{/if}
                      </span>
                      {#if h.detail}<span class="secondary detail">{h.detail}</span>{/if}
                    </div>
                  </div>
                {/each}
              </div>
              <button class="link clear" onclick={async () => (st = await invoke<Status>("clear_history"))}>
                {t.clearHistory}
              </button>
            {/if}
          {/if}
        </div>
      {/key}
    {/if}
  </div>

  <div class="statusbar">
    {#if st && s}
      <span class="status-dot {s.enabled && st.online !== false ? 'live' : ''}"></span>
      <span class="status-text">
        {#if st.stuck}<span class="warn-text">{t.stuck}: {st.jobs}</span>
          <button class="link small" onclick={clearQueue}>{t.clearQueue}</button>
        {:else if st.jobs > 0}{t.inQueue}: {st.jobs}
        {:else if st.busy}{t.printing}
        {:else if s.enabled}{t.ready}
        {:else}{t.off}{/if}
      </span>
      <span class="status-right">
        {#if st.nextRun}{t.nextIn} {st.nextRun <= st.now ? "…" : fmtDur(st.nextRun - st.now)}
        {:else}{t.last}: {s.lastRun ? fmtDate(s.lastRun) : t.never}{/if}
      </span>
    {/if}
  </div>
</main>

<style>
  .window {
    width: 100%;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 14px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    color: var(--text);
  }

  .titlebar {
    position: relative;
    height: 34px;
    display: flex;
    align-items: center;
    flex-shrink: 0;
  }
  .title {
    position: absolute;
    left: 0;
    right: 0;
    text-align: center;
    font-size: 13px;
    font-weight: 600;
    color: var(--text-dim);
    pointer-events: none;
  }
  :global(html.mac) .window {
    border-radius: 0;
    border: none;
  }

  .win-controls {
    position: absolute;
    right: 0;
    top: 0;
    height: 100%;
    display: flex;
  }
  .win-btn {
    width: 44px;
    height: 100%;
    border: none;
    background: transparent;
    color: var(--text-dim);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: background 0.12s ease, color 0.12s ease;
  }
  .win-btn:hover {
    background: var(--ctl-hover);
    color: var(--text);
  }
  .win-btn.close:hover {
    background: #e81123;
    color: #fff;
  }
  .win-btn.close {
    border-top-right-radius: 13px;
  }

  .icon-btn {
    position: absolute;
    top: 0;
    height: 100%;
    width: 44px;
    border: none;
    background: transparent;
    color: var(--text-dim);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: background 0.12s ease, color 0.12s ease;
  }
  .icon-btn:hover {
    background: var(--ctl-hover);
    color: var(--text);
  }
  .gear.win {
    left: 0;
    border-top-left-radius: 13px;
  }
  .gear.mac {
    right: 0;
    border-top-right-radius: 13px;
  }

  .content {
    padding: 10px 16px 16px;
  }
  .view-wrap {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .card {
    width: 100%;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 15px 16px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    cursor: pointer;
    text-align: left;
    color: inherit;
    font: inherit;
    transition: background 0.15s ease, transform 0.08s ease;
  }
  .card.slim {
    padding: 12px 16px;
  }
  .card:hover {
    background: var(--card-hover);
  }
  .card:active {
    transform: scale(0.99);
  }
  .card.static,
  .card.static:hover {
    cursor: default;
    background: var(--card);
    transform: none;
  }

  .card.group {
    padding: 0;
    overflow: hidden;
    display: block;
  }
  .card.group:hover {
    background: var(--card);
  }
  .card.group:active {
    transform: none;
  }
  .group .row {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 16px;
    background: transparent;
    border: none;
    color: inherit;
    font: inherit;
    text-align: start;
    cursor: pointer;
    transition: background 0.12s ease;
  }
  .group .row:hover:not(:disabled) {
    background: var(--card-hover);
  }
  .group .row:disabled {
    cursor: default;
    opacity: 0.7;
  }
  .group .row.static {
    cursor: default;
  }
  .group .row.static:hover {
    background: transparent;
  }
  .group .row.sub {
    border-top: 1px solid var(--border);
  }
  .group .row.sub .primary {
    font-size: 13.5px;
  }
  .group .row:focus-visible {
    outline-offset: -3px;
  }

  .power-left {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--text-dim);
    flex-shrink: 0;
    transition: background 0.2s ease;
  }
  .dot.live {
    background: var(--green);
    box-shadow: 0 0 10px var(--green);
  }
  .mini-dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--text-dim);
    flex-shrink: 0;
  }
  .mini-dot.ok {
    background: var(--green);
  }
  .mini-dot.bad {
    background: #ff453a;
  }
  .net {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .labels {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .primary {
    font-size: 14.5px;
    font-weight: 600;
    color: var(--text);
  }
  .secondary {
    font-size: 11.5px;
    color: var(--text-dim);
  }

  .switch {
    width: 44px;
    height: 26px;
    border-radius: 13px;
    background: var(--track);
    flex-shrink: 0;
    position: relative;
    transition: background 0.2s ease;
  }
  .switch.checked {
    background: var(--accent);
  }
  .knob {
    position: absolute;
    top: 3px;
    inset-inline-start: 3px;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.3);
    transition: transform 0.2s cubic-bezier(0.4, 0, 0.2, 1);
  }
  .switch.checked .knob {
    transform: translateX(18px);
  }
  :global(html[dir="rtl"]) .switch.checked .knob {
    transform: translateX(-18px);
  }
  :global(html[dir="rtl"]) .chev-right {
    transform: scaleX(-1);
  }

  .picker {
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: 190px;
    min-width: 0;
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 6px 8px 6px 10px;
    font: inherit;
    font-size: 12.5px;
    font-weight: 500;
    cursor: pointer;
    transition: border-color 0.15s ease, background 0.15s ease;
  }
  .picker:hover {
    background: var(--card-hover);
  }
  .picker.open {
    border-color: var(--accent);
  }
  .picker-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chev {
    flex-shrink: 0;
    color: var(--text-dim);
    transition: transform 0.18s ease;
  }
  .picker.open .chev {
    transform: rotate(180deg);
    color: var(--accent);
  }
  .plist {
    overflow: hidden;
  }
  .popt .primary {
    font-size: 13px;
  }
  .popt.selected .primary {
    color: var(--accent);
  }
  .check {
    flex-shrink: 0;
    color: var(--accent);
  }

  .section-label {
    font-size: 11px;
    font-weight: 600;
    color: var(--text-dim);
    margin: 4px 2px -2px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .presets {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 7px;
  }
  .presets.five {
    grid-template-columns: repeat(4, 1fr) 1.15fr;
  }
  .preset {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 9px 4px;
    font: inherit;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-dim);
    cursor: pointer;
    text-align: center;
    transition: background 0.15s ease, color 0.15s ease, transform 0.08s ease;
  }
  .preset:hover {
    background: var(--card-hover);
  }
  .preset:active {
    transform: scale(0.96);
  }
  .preset.active {
    background: var(--accent-soft);
    color: var(--accent);
    border-color: var(--accent);
  }
  .preset.time {
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .preset.time {
    gap: 5px;
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }
  .preset.time svg {
    flex-shrink: 0;
    color: var(--text-dim);
  }
  .preset.time.active svg {
    color: var(--accent);
  }
  .freq {
    display: flex;
    flex-direction: column;
  }
  /* A margin rather than the column gap, so it animates with the panel. */
  .picker-card {
    margin-top: 7px;
    padding: 10px 12px;
  }
  .group .row.picker-row {
    padding-top: 10px;
  }

  .segs {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 7px;
  }
  .seg {
    display: flex;
    flex-direction: column;
    gap: 3px;
    align-items: flex-start;
    text-align: start;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 10px 12px;
    font: inherit;
    cursor: pointer;
    transition: background 0.15s ease;
  }
  .seg:hover {
    background: var(--card-hover);
  }
  .seg .primary {
    font-size: 13.5px;
  }
  .seg.active {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .seg.active .primary {
    color: var(--accent);
  }

  .actions {
    display: grid;
    grid-template-columns: 1.4fr 1fr;
    gap: 7px;
    margin-top: 2px;
  }
  .btn {
    border-radius: 10px;
    padding: 10px 8px;
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    background: var(--card);
    color: var(--text);
    border: 1px solid var(--border);
    cursor: pointer;
    transition: background 0.15s ease, transform 0.08s ease, filter 0.15s ease;
  }
  .btn:hover:not(:disabled) {
    background: var(--card-hover);
  }
  .btn:active:not(:disabled) {
    transform: scale(0.97);
  }
  .btn.accent {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .btn.accent:hover:not(:disabled) {
    background: var(--accent);
    filter: brightness(1.08);
  }
  .btn.warn {
    color: #ff9f0a;
    border-color: #ff9f0a;
  }
  .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .error {
    font-size: 11.5px;
    color: #ff453a;
    padding: 0 2px;
    word-break: break-word;
  }

  .chip {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 5px 9px;
    font: inherit;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-dim);
    cursor: pointer;
  }
  .chip.active {
    background: var(--accent-soft);
    color: var(--accent);
    border-color: var(--accent);
  }

  /* Language sub-view: a scrollable list that never stretches the window. */
  .lang-list-full {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-height: 300px;
    overflow-y: auto;
    padding: 2px;
  }
  .lang-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    text-align: start;
    background: var(--card);
    border: 1px solid var(--border);
    color: var(--text);
    font: inherit;
    font-size: 13.5px;
    padding: 12px 14px;
    border-radius: 10px;
    cursor: pointer;
    transition:
      background 0.12s ease,
      color 0.12s ease;
  }
  .lang-item:hover {
    background: var(--card-hover);
  }
  .lang-item.active {
    color: var(--accent);
    border-color: var(--accent);
    background: var(--accent-soft);
    font-weight: 600;
  }
  .lang-list-full::-webkit-scrollbar {
    width: 8px;
  }
  .lang-list-full::-webkit-scrollbar-thumb {
    background: var(--track);
    border-radius: 4px;
  }
  .lang-list-full::-webkit-scrollbar-track {
    background: transparent;
  }

  .lang-current {
    display: flex;
    align-items: center;
    gap: 5px;
    color: var(--text-dim);
    font-size: 13px;
    font-weight: 500;
  }

  .history {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-height: 340px;
    overflow-y: auto;
    padding: 2px;
  }
  .history::-webkit-scrollbar {
    width: 8px;
  }
  .history::-webkit-scrollbar-thumb {
    background: var(--track);
    border-radius: 4px;
  }
  .h-item {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 10px 12px;
  }
  .h-item .mini-dot {
    margin-top: 6px;
  }
  .h-item .primary {
    font-size: 13px;
  }
  .detail {
    word-break: break-word;
  }
  .clear {
    align-self: flex-start;
    padding: 2px;
  }

  .note {
    font-size: 12px;
    line-height: 1.45;
    color: var(--text-dim);
    padding: 2px;
  }
  .note.empty {
    padding: 24px 2px;
    text-align: center;
  }
  .footer {
    font-size: 11px;
    color: var(--text-dim);
    padding: 2px;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    font-size: 12.5px;
    color: var(--accent);
    font-weight: 600;
    cursor: pointer;
  }
  .footer .link {
    font-size: inherit;
  }
  .link:hover {
    text-decoration: underline;
  }

  .refresh {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    margin-inline-start: 2px;
    padding: 0;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .refresh:hover:not(:disabled) {
    background: var(--ctl-hover);
    color: var(--text);
  }
  .refresh:disabled {
    cursor: default;
  }
  .refresh.spin svg {
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .link.small {
    font-size: 11.5px;
  }
  .link:disabled {
    color: var(--text-dim);
    cursor: default;
    text-decoration: none;
  }
  .warn-text {
    color: #ff9f0a;
  }

  .ink-head {
    padding-bottom: 4px !important;
  }
  .tanks {
    display: flex;
    justify-content: space-between;
    gap: 6px;
    padding: 6px 16px 12px;
  }
  .tank {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    min-width: 0;
  }
  .tank-body {
    width: 100%;
    max-width: 34px;
    height: 46px;
    border-radius: 6px;
    background: var(--track);
    border: 1px solid var(--border);
    overflow: hidden;
    display: flex;
    align-items: flex-end;
  }
  .tank-body.low {
    border-color: #ff9f0a;
  }
  .tank-fill {
    width: 100%;
    transition: height 0.4s ease;
  }
  .tank-name {
    font-size: 11px;
    font-weight: 600;
    color: var(--text);
  }
  .tank-pct {
    font-size: 10.5px;
    color: var(--text-dim);
  }
  .meta {
    justify-content: flex-start !important;
    gap: 16px !important;
  }
  .meta b {
    color: var(--text);
    font-weight: 600;
  }
  .meta b.warn-text {
    color: #ff9f0a;
  }

  .quiet-range {
    justify-content: flex-start !important;
    gap: 8px !important;
  }
  .time-chip {
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 5px 10px;
    font: inherit;
    font-size: 12.5px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    cursor: pointer;
    transition: border-color 0.15s ease, background 0.15s ease;
  }
  .time-chip:hover {
    background: var(--card-hover);
  }
  .time-chip.open {
    border-color: var(--accent);
    color: var(--accent);
  }
  .group .row.column {
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
  }
  .weekdays {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 5px;
  }
  .weekdays .chip {
    padding: 5px 0;
  }

  .update-action {
    font-size: 12.5px;
    font-weight: 500;
    color: var(--text-dim);
  }
  .update-action.accent {
    color: var(--accent);
    font-weight: 600;
  }
  .card:disabled {
    opacity: 0.65;
    cursor: default;
  }

  .statusbar {
    height: 42px;
    flex-shrink: 0;
    border-top: 1px solid var(--border);
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 16px;
    font-size: 12px;
  }
  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-dim);
  }
  .status-dot.live {
    background: var(--green);
  }
  .status-text {
    color: var(--text);
    font-weight: 500;
  }
  .status-right {
    margin-inline-start: auto;
    color: var(--text-dim);
  }
</style>

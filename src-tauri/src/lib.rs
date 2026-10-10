mod autostart;
mod printer;
mod schedule;
mod snmp;
mod supplies;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry,
};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_store::StoreExt;

use printer::PrinterInfo;
use supplies::Supplies;

const STORE_FILE: &str = "settings.json";
/// Name of the value in the `Run` registry key (same as the old plugin used).
const AUTOSTART_NAME: &str = "Print Pulse";
const TRAY_NORMAL: &[u8] = include_bytes!("../icons/tray-normal.png");
const TRAY_ACTIVE: &[u8] = include_bytes!("../icons/tray-active.png");
const HISTORY_LEN: usize = 40;
/// How long to wait before retrying a scheduled print when the printer is off.
const OFFLINE_RETRY_SECS: i64 = 30 * 60;
const ONLINE_POLL_SECS: u64 = 30;
const JOB_POLL_SECS: u64 = 3;
/// Before a scheduled print finds the printer offline: wake attempts and the gap between them.
const WAKE_ATTEMPTS: u32 = 4;
const WAKE_GAP: Duration = Duration::from_secs(15);
/// Jobs that sit in a broken queue this long count as stuck.
const STUCK_SECS: i64 = 10 * 60;
const SUPPLIES_REFRESH_SECS: i64 = 15 * 60;
const LOW_INK_PCT: i32 = 15;
const WASTE_WARN_PCT: i32 = 90;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Default, Debug)]
#[serde(rename_all = "lowercase")]
enum Mode {
    /// Printer's own nozzle check pattern via a RAW remote-mode command.
    #[default]
    Nozzle,
    /// Colour swatch page drawn through the regular driver.
    Page,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Nozzle,
    Page,
    Clean,
}

impl Action {
    fn key(self) -> &'static str {
        match self {
            Action::Nozzle => "nozzle",
            Action::Page => "page",
            Action::Clean => "clean",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryEntry {
    ts: i64,
    /// nozzle | page | clean | offline | cleared
    kind: String,
    auto: bool,
    ok: bool,
    detail: String,
}

/// Everything the user configures, plus the bits of history that drive the
/// schedule. Persisted as one JSON value in the store.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    enabled: bool,
    printer: Option<String>,
    mode: Mode,
    interval_days: u32,
    /// Minutes after local midnight.
    time_minute: u32,
    /// Count any print from this PC as maintenance and push the schedule back.
    smart_skip: bool,
    start_minimized: bool,
    /// Launch at login, as the user chose it. `None` until first decided —
    /// then it is taken from whatever the registry has.
    autostart: Option<bool>,
    /// Weekdays without scheduled prints, bit 0 = Monday.
    quiet_days: u8,
    quiet_hours: bool,
    /// Quiet window in minutes after midnight; may wrap past midnight.
    quiet_from: u32,
    quiet_to: u32,
    last_run: Option<i64>,
    last_activity: Option<i64>,
    history: Vec<HistoryEntry>,
    /// Last known consumables, kept so the UI has them while the printer is off.
    supplies: Option<Supplies>,
    /// Low-ink / waste-box warnings already shown, so each fires once per episode.
    alerted: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            printer: None,
            mode: Mode::Nozzle,
            interval_days: 3,
            time_minute: 12 * 60,
            smart_skip: true,
            start_minimized: false,
            autostart: None,
            quiet_days: 0,
            quiet_hours: false,
            quiet_from: 22 * 60,
            quiet_to: 8 * 60,
            last_run: None,
            last_activity: None,
            history: Vec::new(),
            supplies: None,
            alerted: Vec::new(),
        }
    }
}

impl Settings {
    fn clamp(&mut self) {
        self.interval_days = self.interval_days.clamp(1, 60);
        self.time_minute %= 24 * 60;
        self.quiet_days &= 0x7f;
        self.quiet_from %= 24 * 60;
        self.quiet_to %= 24 * 60;
    }

    fn quiet(&self) -> schedule::Quiet {
        schedule::Quiet {
            days: self.quiet_days,
            hours: self.quiet_hours.then_some((self.quiet_from, self.quiet_to)),
        }
    }

    fn schedule_base(&self) -> Option<i64> {
        let activity = if self.smart_skip { self.last_activity } else { None };
        match (self.last_run, activity) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    fn push_history(&mut self, e: HistoryEntry) {
        self.history.insert(0, e);
        self.history.truncate(HISTORY_LEN);
    }
}

/// Volatile state that only lives while the app runs.
#[derive(Default)]
struct Runtime {
    online: Option<bool>,
    jobs: u32,
    retry_at: Option<i64>,
    busy: bool,
    /// Since when the queue has been non-empty.
    queue_since: Option<i64>,
    stuck: bool,
    /// VPN tunnel that is up while the printer is unreachable — the likely cause.
    vpn: Option<String>,
}

#[derive(Default)]
struct Core {
    settings: Mutex<Settings>,
    rt: Mutex<Runtime>,
    /// Ask the network watcher to re-check right away (e.g. printer changed).
    recheck: AtomicBool,
    /// Ask the network watcher to re-read consumables (e.g. after printing).
    refresh_supplies: AtomicBool,
}

type SharedCore = Arc<Core>;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Status {
    settings: Settings,
    next_run: Option<i64>,
    online: Option<bool>,
    jobs: u32,
    busy: bool,
    stuck: bool,
    vpn: Option<String>,
    now: i64,
}

fn next_run(s: &Settings, rt: &Runtime) -> Option<i64> {
    if !s.enabled || s.printer.is_none() {
        return None;
    }
    let planned = schedule::next_run(s.schedule_base(), s.interval_days, s.time_minute);
    let planned = match rt.retry_at {
        Some(r) if r > planned => r,
        _ => planned,
    };
    // A missed run is due now — unless now is quiet time.
    Some(s.quiet().first_allowed(planned.max(schedule::now()), s.time_minute))
}

fn snapshot(core: &Core) -> Status {
    let s = core.settings.lock().unwrap().clone();
    let rt = core.rt.lock().unwrap();
    Status {
        next_run: next_run(&s, &rt),
        online: rt.online,
        jobs: rt.jobs,
        busy: rt.busy,
        stuck: rt.stuck,
        vpn: rt.vpn.clone(),
        now: schedule::now(),
        settings: s,
    }
}

/// Localizable strings for the tray and notifications, pushed from the frontend.
#[derive(Clone, Deserialize)]
struct Labels {
    status_on: String,
    status_off: String,
    enable: String,
    disable: String,
    print_now: String,
    show: String,
    quit: String,
    tooltip_on: String,
    tooltip_off: String,
    done_title: String,
    done_body: String,
    fail_title: String,
    offline_title: String,
    offline_body: String,
    page_title: String,
    page_subtitle: String,
    stuck_title: String,
    stuck_body: String,
    ink_title: String,
    waste_title: String,
    waste_body: String,
    /// Localized ink names by colour key.
    inks: HashMap<String, String>,
}

impl Default for Labels {
    fn default() -> Self {
        Self {
            status_on: "● Autoprint on".into(),
            status_off: "○ Autoprint off".into(),
            enable: "Enable".into(),
            disable: "Disable".into(),
            print_now: "Print now".into(),
            show: "Show window".into(),
            quit: "Quit".into(),
            tooltip_on: "Print Pulse — on".into(),
            tooltip_off: "Print Pulse — off".into(),
            done_title: "Maintenance print sent".into(),
            done_body: "The printer will run its nozzle check.".into(),
            fail_title: "Maintenance print failed".into(),
            offline_title: "Printer is offline".into(),
            offline_body: "Will try again in 30 minutes.".into(),
            page_title: "Print Pulse — maintenance print".into(),
            page_subtitle: "Keeps the print head from drying out.".into(),
            stuck_title: "Print queue is stuck".into(),
            stuck_body: "Jobs have been waiting for a while. Open Print Pulse to clear the queue.".into(),
            ink_title: "Ink is running low".into(),
            waste_title: "Maintenance box is almost full".into(),
            waste_body: "Replace it soon, or the printer will stop with a service error.".into(),
            inks: HashMap::new(),
        }
    }
}

fn labels(app: &AppHandle) -> Labels {
    app.try_state::<Mutex<Labels>>()
        .and_then(|s| s.lock().ok().map(|l| l.clone()))
        .unwrap_or_default()
}

struct TrayUi {
    status: MenuItem<Wry>,
    toggle: MenuItem<Wry>,
    print: MenuItem<Wry>,
    show: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

fn update_status_ui(app: &AppHandle, enabled: bool) {
    let l = labels(app);
    let bytes = if enabled { TRAY_ACTIVE } else { TRAY_NORMAL };
    if let Some(tray) = app.tray_by_id("main-tray") {
        if let Ok(img) = Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(img));
        }
        let _ = tray.set_tooltip(Some(if enabled { &l.tooltip_on } else { &l.tooltip_off }));
    }
    if let Some(win) = app.get_webview_window("main") {
        if let Ok(img) = Image::from_bytes(bytes) {
            let _ = win.set_icon(img);
        }
    }
    if let Some(state) = app.try_state::<Mutex<TrayUi>>() {
        if let Ok(ui) = state.lock() {
            let _ = ui.status.set_text(if enabled { &l.status_on } else { &l.status_off });
            let _ = ui.toggle.set_text(if enabled { &l.disable } else { &l.enable });
        }
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

fn persist(app: &AppHandle, core: &Core) {
    let s = core.settings.lock().unwrap().clone();
    if let Ok(store) = app.store(STORE_FILE) {
        store.set("settings", json!(s));
        let _ = store.save();
    }
}

fn emit_status(app: &AppHandle, core: &Core) {
    let _ = app.emit("status", snapshot(core));
}

fn host_of(name: &str) -> Option<String> {
    printer::list_printers().into_iter().find(|p| p.name == name).and_then(|p| p.host)
}

/// Run one maintenance action on a background thread and record the outcome.
fn start_job(app: AppHandle, core: SharedCore, action: Action, auto: bool) -> Result<(), String> {
    let name = core
        .settings
        .lock()
        .unwrap()
        .printer
        .clone()
        .ok_or("no printer selected")?;
    {
        let mut rt = core.rt.lock().unwrap();
        if rt.busy {
            return Err("busy".into());
        }
        rt.busy = true;
    }
    emit_status(&app, &core);

    std::thread::spawn(move || {
        let l = labels(&app);
        let now = schedule::now();

        // A scheduled run against a powered-off network printer would just sit in
        // the queue; try to wake it, otherwise postpone so the history stays truthful.
        // USB / WSD printers have no address to probe — then we trust the spooler.
        let asleep = auto && host_of(&name).is_some_and(|h| !printer::wake(&h, WAKE_ATTEMPTS, WAKE_GAP));
        if asleep {
            {
                let mut s = core.settings.lock().unwrap();
                s.push_history(HistoryEntry {
                    ts: now,
                    kind: "offline".into(),
                    auto,
                    ok: false,
                    detail: String::new(),
                });
            }
            {
                let mut rt = core.rt.lock().unwrap();
                rt.online = Some(false);
                rt.retry_at = Some(now + OFFLINE_RETRY_SECS);
                rt.busy = false;
            }
            persist(&app, &core);
            notify(&app, &l.offline_title, &l.offline_body);
            emit_status(&app, &core);
            return;
        }

        let result = match action {
            Action::Nozzle => printer::send_raw(&name, "Print Pulse — nozzle check", &printer::nozzle_check_bytes()),
            Action::Clean => printer::send_raw(&name, "Print Pulse — head cleaning", &printer::head_cleaning_bytes()),
            Action::Page => {
                let when = chrono::Local::now().format("%d.%m.%Y %H:%M");
                printer::print_test_page(&name, &l.page_title, &format!("{} · {}", l.page_subtitle, when))
            }
        };

        {
            let mut s = core.settings.lock().unwrap();
            if result.is_ok() {
                s.last_run = Some(now);
            }
            s.push_history(HistoryEntry {
                ts: now,
                kind: action.key().into(),
                auto,
                ok: result.is_ok(),
                detail: result.clone().err().unwrap_or_default(),
            });
            let mut rt = core.rt.lock().unwrap();
            rt.busy = false;
            if result.is_ok() {
                rt.retry_at = None;
            } else if auto {
                rt.retry_at = Some(now + OFFLINE_RETRY_SECS);
            }
        }
        persist(&app, &core);
        match &result {
            Ok(_) if auto => notify(&app, &l.done_title, &l.done_body),
            Err(e) => notify(&app, &l.fail_title, e),
            _ => {}
        }
        emit_status(&app, &core);
    });
    Ok(())
}

/// Called when the printer's queue drains: any print counts as activity, and
/// the ink levels have changed.
fn on_queue_drained(app: &AppHandle, core: &Core) {
    core.settings.lock().unwrap().last_activity = Some(schedule::now());
    core.refresh_supplies.store(true, Ordering::Relaxed);
    persist(app, core);
}

/// Compare fresh consumables with what was already reported and notify once
/// per new problem. Refilled inks drop out of `alerted`, so they can warn again.
fn check_supply_alerts(app: &AppHandle, core: &Core) {
    let l = labels(app);
    let (new_inks, waste_new) = {
        let mut s = core.settings.lock().unwrap();
        let Some(sup) = s.supplies.clone() else { return };
        let mut current: Vec<String> = Vec::new();
        let mut new_inks: Vec<String> = Vec::new();
        for ink in &sup.inks {
            let Some(level) = ink.level.filter(|v| *v <= LOW_INK_PCT) else { continue };
            let id = format!("ink:{}:{}", ink.key, ink.label);
            if !s.alerted.contains(&id) {
                let name = l.inks.get(&ink.key).cloned().unwrap_or_else(|| ink.label.clone());
                new_inks.push(format!("{name} — {level}%"));
            }
            current.push(id);
        }
        let waste_bad = sup.waste_pct.is_some_and(|p| p >= WASTE_WARN_PCT)
            || matches!(sup.waste_state.as_deref(), Some("near" | "full"));
        let waste_new = waste_bad && !s.alerted.iter().any(|a| a == "waste");
        if waste_bad {
            current.push("waste".into());
        }
        s.alerted = current;
        (new_inks, waste_new)
    };
    persist(app, core);
    if !new_inks.is_empty() {
        notify(app, &l.ink_title, &new_inks.join(", "));
    }
    if waste_new {
        notify(app, &l.waste_title, &l.waste_body);
    }
}

/// Heartbeat: once a second it publishes status and fires due jobs; every few
/// seconds it watches the print queue.
fn spawn_loop(app: AppHandle, core: SharedCore) {
    std::thread::spawn(move || {
        let mut tick: u64 = 0;
        let mut prev_jobs: Option<(String, u32)> = None;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            tick += 1;
            let now = schedule::now();
            let settings = core.settings.lock().unwrap().clone();

            if tick % JOB_POLL_SECS == 0 {
                if let Some(name) = &settings.printer {
                    if let Ok(list) = printer::list_jobs(name) {
                        let jobs = list.len() as u32;
                        let drained = matches!(&prev_jobs, Some((n, p)) if n == name && *p > 0 && jobs == 0);
                        let became_stuck = {
                            let mut rt = core.rt.lock().unwrap();
                            rt.jobs = jobs;
                            let was_stuck = rt.stuck;
                            if jobs > 0 {
                                let since = *rt.queue_since.get_or_insert(now);
                                let broken = rt.online == Some(false) || list.iter().any(|j| j.problem);
                                rt.stuck = broken && now - since >= STUCK_SECS;
                            } else {
                                rt.queue_since = None;
                                rt.stuck = false;
                            }
                            rt.stuck && !was_stuck
                        };
                        if drained {
                            on_queue_drained(&app, &core);
                        }
                        if became_stuck {
                            let l = labels(&app);
                            notify(&app, &l.stuck_title, &l.stuck_body);
                        }
                        prev_jobs = Some((name.clone(), jobs));
                    }
                }
            }

            let due = {
                let rt = core.rt.lock().unwrap();
                !rt.busy && next_run(&settings, &rt).is_some_and(|t| now >= t)
            };
            if due {
                let action = match settings.mode {
                    Mode::Nozzle => Action::Nozzle,
                    Mode::Page => Action::Page,
                };
                let _ = start_job(app.clone(), core.clone(), action, true);
            }

            emit_status(&app, &core);
        }
    });
}

/// Check reachability and record it, together with a VPN that would explain
/// an unreachable printer.
fn probe(core: &Core, host: Option<&str>) -> Option<bool> {
    let online = host.map(printer::is_reachable);
    let vpn = if online == Some(false) { printer::active_vpn() } else { None };
    let mut rt = core.rt.lock().unwrap();
    rt.online = online;
    rt.vpn = vpn;
    online
}

/// Network watcher: keeps `online` and the consumables fresh without
/// blocking the heartbeat. `online` is `None` for printers without a network
/// address (USB / WSD) — then we simply trust the spooler.
fn spawn_online_watch(app: AppHandle, core: SharedCore) {
    std::thread::spawn(move || loop {
        let (name, last_read) = {
            let s = core.settings.lock().unwrap();
            (s.printer.clone(), s.supplies.as_ref().map(|x| x.ts))
        };
        let host = name.as_deref().and_then(host_of);
        let online = probe(&core, host.as_deref());

        let now = schedule::now();
        let stale = last_read.is_none_or(|t| now - t >= SUPPLIES_REFRESH_SECS);
        let asked = core.refresh_supplies.swap(false, Ordering::Relaxed);
        if let (Some(host), Some(true)) = (host.as_deref(), online) {
            if stale || asked {
                if let Some(fresh) = supplies::fetch(host, now) {
                    let mut s = core.settings.lock().unwrap();
                    // The user may have switched printers while we were reading.
                    if s.printer == name {
                        s.supplies = Some(fresh);
                        drop(s);
                        check_supply_alerts(&app, &core);
                        emit_status(&app, &core);
                    }
                }
            }
        }
        for _ in 0..ONLINE_POLL_SECS {
            std::thread::sleep(Duration::from_secs(1));
            if core.recheck.swap(false, Ordering::Relaxed) {
                break;
            }
        }
    });
}

#[tauri::command]
fn get_status(core: State<SharedCore>) -> Status {
    snapshot(&core)
}

#[tauri::command]
fn list_printers() -> Vec<PrinterInfo> {
    printer::list_printers()
}

/// Merge a partial settings object from the UI. History and timestamps are
/// owned by the backend and can't be overwritten this way.
#[tauri::command]
fn patch_settings(patch: Value, app: AppHandle, core: State<SharedCore>) -> Result<Status, String> {
    let Value::Object(patch) = patch else {
        return Err("patch must be an object".into());
    };
    let (enabled, printer_changed, schedule_changed) = {
        let mut s = core.settings.lock().unwrap();
        let before_printer = s.printer.clone();
        let mut v = serde_json::to_value(&*s).map_err(|e| e.to_string())?;
        for (k, val) in patch {
            if matches!(k.as_str(), "history" | "lastRun" | "lastActivity" | "supplies" | "alerted" | "autostart") {
                continue;
            }
            v[k.as_str()] = val;
        }
        let mut next: Settings = serde_json::from_value(v).map_err(|e| e.to_string())?;
        next.clamp();
        let printer_changed = next.printer != before_printer;
        if printer_changed {
            next.supplies = None;
            next.alerted.clear();
        }
        let schedule_changed = next.enabled != s.enabled
            || next.interval_days != s.interval_days
            || next.time_minute != s.time_minute
            || printer_changed;
        *s = next;
        (s.enabled, printer_changed, schedule_changed)
    };
    if schedule_changed {
        core.rt.lock().unwrap().retry_at = None;
    }
    if printer_changed {
        let mut rt = core.rt.lock().unwrap();
        rt.online = None;
        rt.vpn = None;
        rt.jobs = 0;
        rt.queue_since = None;
        rt.stuck = false;
        core.recheck.store(true, Ordering::Relaxed);
    }
    persist(&app, &core);
    update_status_ui(&app, enabled);
    Ok(snapshot(&core))
}

#[tauri::command]
fn print_now(app: AppHandle, core: State<SharedCore>) -> Result<(), String> {
    let action = match core.settings.lock().unwrap().mode {
        Mode::Nozzle => Action::Nozzle,
        Mode::Page => Action::Page,
    };
    start_job(app, core.inner().clone(), action, false)
}

#[tauri::command]
fn run_action(action: String, app: AppHandle, core: State<SharedCore>) -> Result<(), String> {
    let action = match action.as_str() {
        "nozzle" => Action::Nozzle,
        "page" => Action::Page,
        "clean" => Action::Clean,
        other => return Err(format!("unknown action {other}")),
    };
    start_job(app, core.inner().clone(), action, false)
}

/// Cancel everything waiting in the selected printer's queue.
#[tauri::command]
fn clear_queue(app: AppHandle, core: State<SharedCore>) -> Result<Status, String> {
    let name = core.settings.lock().unwrap().printer.clone().ok_or("no printer selected")?;
    let ids: Vec<u32> = printer::list_jobs(&name)?.iter().map(|j| j.id).collect();
    let removed = printer::delete_jobs(&name, &ids)?;
    core.settings.lock().unwrap().push_history(HistoryEntry {
        ts: schedule::now(),
        kind: "cleared".into(),
        auto: false,
        ok: removed as usize == ids.len(),
        detail: format!("{removed}/{}", ids.len()),
    });
    {
        let mut rt = core.rt.lock().unwrap();
        rt.stuck = false;
        rt.queue_since = None;
    }
    persist(&app, &core);
    Ok(snapshot(&core))
}

#[tauri::command]
fn refresh_supplies(core: State<SharedCore>) {
    core.refresh_supplies.store(true, Ordering::Relaxed);
    core.recheck.store(true, Ordering::Relaxed);
}

/// "Refresh" button: probe the printer right now and return the result; the
/// network watcher then re-reads the consumables in the background.
#[tauri::command]
async fn refresh_status(core: State<'_, SharedCore>) -> Result<Status, String> {
    let core = core.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let name = core.settings.lock().unwrap().printer.clone();
        let host = name.as_deref().and_then(host_of);
        if probe(&core, host.as_deref()) == Some(true) {
            core.refresh_supplies.store(true, Ordering::Relaxed);
            core.recheck.store(true, Ordering::Relaxed);
        }
        snapshot(&core)
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn clear_history(app: AppHandle, core: State<SharedCore>) -> Status {
    core.settings.lock().unwrap().history.clear();
    persist(&app, &core);
    snapshot(&core)
}

/// Launch-at-login state for the settings screen.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AutostartView {
    /// What the user chose.
    on: bool,
    /// Switched off in Task Manager / Settings → Startup apps.
    blocked: bool,
    /// Dev build: the registry is left alone (see `sync_autostart`).
    dev: bool,
}

fn autostart_view(core: &Core) -> AutostartView {
    let entry = autostart::read(AUTOSTART_NAME);
    AutostartView {
        on: core.settings.lock().unwrap().autostart.unwrap_or(false),
        blocked: entry.path.is_some() && entry.blocked,
        dev: cfg!(debug_assertions),
    }
}

/// Make the `Run` entry match the user's choice and the exe that is running
/// now. Called on every start, so an entry wiped by a reinstall, or left
/// pointing at another copy, is put right. Dev builds never register
/// themselves — their window loads from a dev server that won't be running
/// at login — and they remove such an entry if an older build left one.
fn sync_autostart(want: Option<bool>) -> Option<bool> {
    let entry = autostart::read(AUTOSTART_NAME);
    let stale_dev = entry.path.as_deref().is_some_and(autostart::is_build_output);
    if cfg!(debug_assertions) {
        if stale_dev {
            let _ = autostart::unregister(AUTOSTART_NAME);
        }
        return want;
    }
    let Ok(exe) = std::env::current_exe() else { return want };
    // First run of this version: adopt what an older one registered.
    let want = want.unwrap_or(entry.path.is_some() && !stale_dev);
    if want && !entry.points_to(&exe) {
        let _ = autostart::register(AUTOSTART_NAME, &exe);
    } else if !want && entry.path.is_some() {
        let _ = autostart::unregister(AUTOSTART_NAME);
    }
    Some(want)
}

#[tauri::command]
fn get_autostart(core: State<SharedCore>) -> AutostartView {
    autostart_view(&core)
}

#[tauri::command]
fn set_autostart(on: bool, app: AppHandle, core: State<SharedCore>) -> Result<AutostartView, String> {
    if !cfg!(debug_assertions) {
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            autostart::register(AUTOSTART_NAME, &exe)?;
        } else {
            autostart::unregister(AUTOSTART_NAME)?;
        }
    }
    core.settings.lock().unwrap().autostart = Some(on);
    persist(&app, &core);
    Ok(autostart_view(&core))
}

#[tauri::command]
fn set_labels(labels: Labels, app: AppHandle, core: State<SharedCore>) {
    if let Some(state) = app.try_state::<Mutex<Labels>>() {
        if let Ok(mut current) = state.lock() {
            *current = labels.clone();
        }
    }
    if let Some(ui) = app.try_state::<Mutex<TrayUi>>() {
        if let Ok(ui) = ui.lock() {
            let _ = ui.show.set_text(&labels.show);
            let _ = ui.print.set_text(&labels.print_now);
            let _ = ui.quit.set_text(&labels.quit);
        }
    }
    update_status_ui(&app, core.settings.lock().unwrap().enabled);
}

fn show_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let core: SharedCore = Arc::new(Core::default());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_window(app);
        }))
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(core.clone())
        .setup(move |app| {
            let handle = app.handle().clone();

            {
                let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("Print Pulse")
                    .inner_size(420.0, 560.0)
                    .resizable(false)
                    .visible(false)
                    .center();

                #[cfg(target_os = "macos")]
                let builder = builder
                    .title_bar_style(tauri::TitleBarStyle::Overlay)
                    .hidden_title(true);

                #[cfg(not(target_os = "macos"))]
                let builder = builder.decorations(false).transparent(true).shadow(false);

                builder.build()?;
            }

            // Restore saved settings; default to the system printer on first run.
            {
                let mut s = app
                    .store(STORE_FILE)
                    .ok()
                    .and_then(|store| store.get("settings"))
                    .and_then(|v| serde_json::from_value::<Settings>(v).ok())
                    .unwrap_or_default();
                if s.printer.is_none() {
                    s.printer = printer::default_printer();
                }
                s.history.retain(|h| h.kind != "tray");
                s.autostart = sync_autostart(s.autostart);
                s.clamp();
                *core.settings.lock().unwrap() = s;
            }

            app.manage(Mutex::new(Labels::default()));

            let status_i = MenuItem::with_id(app, "status", "○ Автопечать выключена", false, None::<&str>)?;
            let print_i = MenuItem::with_id(app, "print", "Напечатать сейчас", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Показать окно", true, None::<&str>)?;
            let toggle_i = MenuItem::with_id(app, "toggle", "Включить", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status_i,
                    &PredefinedMenuItem::separator(app)?,
                    &show_i,
                    &print_i,
                    &toggle_i,
                    &PredefinedMenuItem::separator(app)?,
                    &quit_i,
                ],
            )?;

            let tray_core = core.clone();
            TrayIconBuilder::with_id("main-tray")
                .icon(Image::from_bytes(TRAY_NORMAL)?)
                .tooltip("Print Pulse")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => show_window(app),
                    "print" => {
                        let action = match tray_core.settings.lock().unwrap().mode {
                            Mode::Nozzle => Action::Nozzle,
                            Mode::Page => Action::Page,
                        };
                        let _ = start_job(app.clone(), tray_core.clone(), action, false);
                    }
                    "toggle" => {
                        let enabled = {
                            let mut s = tray_core.settings.lock().unwrap();
                            s.enabled = !s.enabled;
                            s.enabled
                        };
                        tray_core.rt.lock().unwrap().retry_at = None;
                        persist(app, &tray_core);
                        update_status_ui(app, enabled);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => show_window(tray.app_handle()),
                    _ => {}
                })
                .build(app)?;

            app.manage(Mutex::new(TrayUi {
                status: status_i,
                toggle: toggle_i,
                print: print_i,
                show: show_i,
                quit: quit_i,
            }));

            spawn_online_watch(handle.clone(), core.clone());
            spawn_loop(handle.clone(), core.clone());

            let (enabled, start_min) = {
                let s = core.settings.lock().unwrap();
                (s.enabled, s.start_minimized)
            };
            update_status_ui(&handle, enabled);

            let autostarted = std::env::args().any(|a| a == autostart::ARG);
            if !(autostarted && start_min) {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            list_printers,
            patch_settings,
            print_now,
            run_action,
            clear_history,
            clear_queue,
            refresh_supplies,
            refresh_status,
            get_autostart,
            set_autostart,
            set_labels
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

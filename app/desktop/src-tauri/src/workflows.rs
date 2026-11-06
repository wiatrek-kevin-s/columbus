//! Workflows in the app: the list, approving one (Touch ID / Windows
//! Hello), its schedule (a launchd agent on macOS, a Task Scheduler task on
//! Windows), Run now, and its runs' reports.
//!
//! The schedule entry only names the workflow and the engine: the runner
//! (`columbus-mcp workflow run <id>`) checks everything else itself, so an
//! entry that was changed or left behind cannot run anything unapproved.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use columbus_shared::workflows as wf;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::settings::{confirm, policy_now};

static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tools_root() -> PathBuf {
    columbus_shared::tools::tools_dir()
}

#[derive(Serialize)]
pub struct WorkflowsState {
    dir: String,
    /// The organization allows workflows (`allow_workflows`).
    allowed: bool,
    /// How schedules are kept here: `launchd`, `task-scheduler`, or none.
    scheduler: Option<&'static str>,
    installed: Vec<Value>,
    waiting: Vec<wf::WorkflowView>,
}

#[tauri::command]
pub async fn workflows_state(app: AppHandle) -> Result<WorkflowsState, String> {
    let program = runner(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let root = wf::workflows_dir();
        let installed = wf::list_installed(&root, &tools_root())
            .into_iter()
            .map(|v| {
                let entry = schedule_state(&v, program.as_ref().map(|p| &p.0));
                let mut j = serde_json::to_value(&v).unwrap_or(Value::Null);
                j["schedule_entry"] = json!(entry);
                j
            })
            .collect();
        WorkflowsState {
            dir: root.display().to_string(),
            allowed: policy_now().allow_workflows,
            scheduler: scheduler_kind(),
            installed,
            waiting: wf::list_proposals(&root, &tools_root()),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// The server program and engine a run uses: the chat's.
fn runner(app: &AppHandle) -> Option<(PathBuf, Option<PathBuf>)> {
    let s = crate::chat::load(app);
    let server = crate::chat::server(&s)?;
    let engine = crate::chat::engine_python(&s, Some(&server));
    Some((server, engine))
}

/// Approve a waiting workflow: it will run by itself without asking, so
/// the person confirms first, and exactly what they were shown
/// (`fingerprint`) is installed. Then its schedule is set.
#[tauri::command]
pub async fn workflow_install(
    app: AppHandle,
    id: String,
    fingerprint: String,
) -> Result<(), String> {
    let program = runner(&app);
    tauri::async_runtime::spawn_blocking(move || {
        if !policy_now().allow_workflows {
            return Err("your organization's settings turn workflows off, so none can be approved here (Allow workflows)".into());
        }
        let root = wf::workflows_dir();
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let p = wf::read_proposal(&root, &tools_root(), &id)?;
        if p.fingerprint != fingerprint {
            return Err(
                "the workflow changed after you looked at it; nothing was approved. Look again"
                    .into(),
            );
        }
        let when = match &p.at {
            Some(at) => format!("on its schedule ({} at {at})", days_words(&p.days)),
            None => "when you start it".into(),
        };
        confirm(&format!(
            "approve the workflow \"{}\", which then runs by itself {when}, reading {} and doing exactly what it lists",
            p.name,
            if p.connections.is_empty() {
                "no connection".to_string()
            } else {
                p.connections.join(", ")
            }
        ))?;
        // A version the person had paused stays paused.
        let paused = wf::list_installed(&root, &tools_root())
            .into_iter()
            .find(|w| w.id == id)
            .is_some_and(|w| w.at.is_some() && schedule_state(&w, program.as_ref().map(|p| &p.0)) == "off");
        // Checked again on the file itself: what was shown is what goes in.
        let v = wf::install(&root, &tools_root(), &id, &fingerprint, &columbus_shared::tools::now())?;
        // No entry from an earlier version may outlive it: the new one
        // runs on its own schedule or none.
        unschedule(&id)?;
        // Where there is no scheduler yet, the page says so; Run now works.
        if v.at.is_some() && scheduler_kind().is_some() && !paused {
            let (server, engine) = program.ok_or(
                "approved, but Columbus's server was not found, so it is not scheduled; Run now works once it is",
            )?;
            if let Err(e) = schedule(&v, &server, engine.as_deref()) {
                let _ = unschedule(&id);
                return Err(format!("approved, but its schedule could not be set, so it runs only when you start it: {e}"));
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Decline a waiting workflow: it is deleted.
#[tauri::command]
pub async fn workflow_decline(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        wf::decline(&wf::workflows_dir(), &id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove an installed workflow: its schedule first, then the workflow.
/// Its past runs stay readable.
#[tauri::command]
pub async fn workflow_remove(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        if !wf::id_ok(&id) {
            return Err(format!("{id:?} is not a workflow"));
        }
        unschedule(&id)?;
        wf::remove(&wf::workflows_dir(), &id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Pause (remove the schedule entry) or resume (set it again) a workflow's
/// schedule. Resuming widens nothing: it is what the person approved.
#[tauri::command]
pub async fn workflow_set_schedule(app: AppHandle, id: String, on: bool) -> Result<(), String> {
    let program = runner(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        if !wf::id_ok(&id) {
            return Err(format!("{id:?} is not a workflow"));
        }
        if !on {
            return unschedule(&id);
        }
        let root = wf::workflows_dir();
        wf::ready(&root, &tools_root(), &id)?;
        let v = wf::list_installed(&root, &tools_root())
            .into_iter()
            .find(|v| v.id == id)
            .ok_or("no such workflow")?;
        if v.at.is_none() {
            return Err("this workflow has no schedule; use Run now".into());
        }
        let (server, engine) = program.ok_or("Columbus's server was not found")?;
        schedule(&v, &server, engine.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Run a workflow now, as its schedule would, and wait for it to finish.
#[tauri::command]
pub async fn workflow_run_now(app: AppHandle, id: String) -> Result<Value, String> {
    let (server, engine) = runner(&app).ok_or("Columbus's server was not found")?;
    tauri::async_runtime::spawn_blocking(move || {
        if !wf::id_ok(&id) {
            return Err(format!("{id:?} is not a workflow"));
        }
        let root = wf::workflows_dir();
        let (w, _) = wf::ready(&root, &tools_root(), &id)?;
        let mut cmd = Command::new(&server);
        cmd.args(["workflow", "run", &id])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(e) = &engine {
            cmd.env("COLUMBUS_PYTHON", e);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("{}: {e}", server.display()))?;
        let limit = Duration::from_secs(u64::from(w.run_minutes()) * 60 + 120);
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < limit => {
                    std::thread::sleep(Duration::from_millis(200))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("the run did not finish in its time limit".into());
                }
            }
        }
        wf::list_runs(&root, &id, 1)
            .into_iter()
            .next()
            .ok_or_else(|| "the run left no record".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A workflow's latest runs, newest first.
#[tauri::command]
pub async fn workflow_runs(id: String) -> Result<Vec<Value>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !wf::id_ok(&id) {
            return Err(format!("{id:?} is not a workflow"));
        }
        Ok(wf::list_runs(&wf::workflows_dir(), &id, 50))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One run's record and report.
#[tauri::command]
pub async fn workflow_run_report(id: String, run: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (rec, report) = wf::read_run(&wf::workflows_dir(), &id, &run)?;
        Ok(json!({"record": rec, "report": report}))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn days_words(days: &[String]) -> String {
    match days.len() {
        7 => "every day".into(),
        5 if days
            .iter()
            .all(|d| ["mon", "tue", "wed", "thu", "fri"].contains(&d.as_str())) =>
        {
            "weekdays".into()
        }
        _ => days.join(", "),
    }
}

// ---------- The schedule entry ----------

fn scheduler_kind() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("launchd")
    } else if cfg!(windows) {
        Some("task-scheduler")
    } else {
        None
    }
}

/// The program and its arguments a schedule entry runs.
fn entry_args(id: &str, engine: Option<&Path>) -> Vec<String> {
    let mut a = vec![
        "workflow".to_string(),
        "run".into(),
        id.into(),
        "--scheduled".into(),
    ];
    if let Some(e) = engine {
        a.push("--python".into());
        a.push(e.display().to_string());
    }
    a
}

const DAY_NUMBERS: &[(&str, u8)] = &[
    ("sun", 0),
    ("mon", 1),
    ("tue", 2),
    ("wed", 3),
    ("thu", 4),
    ("fri", 5),
    ("sat", 6),
];

fn label(id: &str) -> String {
    format!("io.columbus.workflow.{id}")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The launchd agent for a workflow (macOS): runs at each scheduled day
/// and time; launchd runs a missed one once when the Mac wakes.
pub(crate) fn launchd_plist(v: &wf::WorkflowView, server: &Path, engine: Option<&Path>) -> String {
    let (h, m) =
        v.at.as_deref()
            .and_then(|a| a.split_once(':'))
            .map(|(h, m)| (h.parse::<u8>().unwrap_or(0), m.parse::<u8>().unwrap_or(0)))
            .unwrap_or((0, 0));
    let mut args = format!(
        "    <string>{}</string>\n",
        xml_escape(&server.display().to_string())
    );
    for a in entry_args(&v.id, engine) {
        args.push_str(&format!("    <string>{}</string>\n", xml_escape(&a)));
    }
    let mut times = String::new();
    for d in &v.days {
        if let Some((_, n)) = DAY_NUMBERS.iter().find(|(name, _)| name == d) {
            times.push_str(&format!(
                "    <dict><key>Weekday</key><integer>{n}</integer><key>Hour</key><integer>{h}</integer><key>Minute</key><integer>{m}</integer></dict>\n"
            ));
        }
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n{args}  </array>\n  <key>StartCalendarInterval</key>\n  <array>\n{times}  </array>\n  <key>ProcessType</key><string>Background</string>\n  <key>LowPriorityIO</key><true/>\n</dict>\n</plist>\n",
        xml_escape(&label(&v.id))
    )
}

/// The Task Scheduler task for a workflow (Windows): weekly on its days at
/// its time, only while the person is signed in, a missed run started as
/// soon as possible, never two at once, stopped at its time limit.
pub(crate) fn task_xml(
    v: &wf::WorkflowView,
    server: &Path,
    engine: Option<&Path>,
    run_minutes: u32,
) -> String {
    let at = v.at.clone().unwrap_or_else(|| "00:00".into());
    let names = [
        ("sun", "Sunday"),
        ("mon", "Monday"),
        ("tue", "Tuesday"),
        ("wed", "Wednesday"),
        ("thu", "Thursday"),
        ("fri", "Friday"),
        ("sat", "Saturday"),
    ];
    let days: String = names
        .iter()
        .filter(|(d, _)| v.days.iter().any(|x| x == d))
        .map(|(_, n)| format!("<{n} />"))
        .collect();
    // The windowless launcher beside the server, when there (no console
    // window for the run); it adds "workflow run" itself.
    let launcher = server.with_file_name("columbus-workflow.exe");
    let (program, args) = if launcher.is_file() {
        (launcher, entry_args(&v.id, engine).split_off(2))
    } else {
        (server.to_path_buf(), entry_args(&v.id, engine))
    };
    // Windows command-line quoting: each argument in double quotes (no path
    // or id here can contain one).
    let args: Vec<String> = args.into_iter().map(|a| format!("\"{a}\"")).collect();
    // Any day in the past: the days of the week and the time are what count.
    let today = "2024-01-01";
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <RegistrationInfo><Description>Columbus workflow {}: runs what you approved in the Columbus app.</Description></RegistrationInfo>\n  <Triggers>\n    <CalendarTrigger>\n      <StartBoundary>{today}T{at}:00</StartBoundary>\n      <Enabled>true</Enabled>\n      <ScheduleByWeek><DaysOfWeek>{days}</DaysOfWeek><WeeksInterval>1</WeeksInterval></ScheduleByWeek>\n    </CalendarTrigger>\n  </Triggers>\n  <Principals><Principal id=\"Author\"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n  <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n    <StartWhenAvailable>true</StartWhenAvailable>\n    <ExecutionTimeLimit>PT{}M</ExecutionTimeLimit>\n    <Enabled>true</Enabled>\n  </Settings>\n  <Actions Context=\"Author\">\n    <Exec><Command>{}</Command><Arguments>{}</Arguments></Exec>\n  </Actions>\n</Task>\n",
        xml_escape(&v.id),
        run_minutes + 5,
        xml_escape(&program.display().to_string()),
        xml_escape(&args.join(" "))
    )
}

fn plist_path(id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
    Some(
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label(id))),
    )
}

/// At the top of the person's task library: a new folder there would need
/// an administrator.
fn task_name(id: &str) -> String {
    format!("Columbus Workflow {id}")
}

fn run_minutes_of(id: &str) -> u32 {
    wf::ready(&wf::workflows_dir(), &tools_root(), id)
        .map(|(w, _)| w.run_minutes())
        .unwrap_or(wf::DEFAULT_RUN_MINUTES)
}

/// Set (or replace) the workflow's schedule entry.
fn schedule(v: &wf::WorkflowView, server: &Path, engine: Option<&Path>) -> Result<(), String> {
    // The engine is named only when the server would not find it itself.
    let engine = engine.filter(|e| {
        server
            .parent()
            .map(|d| d.join("engine"))
            .is_none_or(|d| !e.starts_with(d))
    });
    if cfg!(target_os = "macos") {
        let p = plist_path(&v.id).ok_or("no home folder")?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        columbus_shared::tools::write_atomic(&p, launchd_plist(v, server, engine).as_bytes())?;
        let domain = format!("gui/{}", uid());
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("{domain}/{}", label(&v.id))])
            .output();
        // launchd can refuse a bootstrap right after a bootout; try again.
        let mut last = String::new();
        for _ in 0..5 {
            let out = Command::new("/bin/launchctl")
                .args(["bootstrap", &domain])
                .arg(&p)
                .output()
                .map_err(|e| e.to_string())?;
            if out.status.success() {
                return Ok(());
            }
            last = String::from_utf8_lossy(&out.stderr).trim().to_string();
            std::thread::sleep(Duration::from_millis(400));
        }
        let _ = std::fs::remove_file(&p);
        Err(format!("launchctl: {last}"))
    } else if cfg!(windows) {
        let xml = task_xml(v, server, engine, run_minutes_of(&v.id));
        let dir =
            std::env::temp_dir().join(format!("columbus-task-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join("task.xml");
        // Task Scheduler reads the file as UTF-16, as its header says.
        let mut bytes = vec![0xFF, 0xFE];
        for u in xml.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let written = std::fs::write(&file, bytes).map_err(|e| e.to_string());
        let out = written.and_then(|_| {
            let mut c = Command::new("schtasks.exe");
            c.args(["/Create", "/TN", &task_name(&v.id), "/XML"])
                .arg(&file)
                .arg("/F");
            hide(&mut c);
            c.output().map_err(|e| e.to_string())
        });
        let _ = std::fs::remove_dir_all(&dir);
        let out = out?;
        if !out.status.success() {
            return Err(format!(
                "Task Scheduler: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    } else {
        Err("schedules are not available on this system yet; use Run now".into())
    }
}

/// Remove the workflow's schedule entry (none there is fine).
fn unschedule(id: &str) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", uid());
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("{domain}/{}", label(id))])
            .output();
        if let Some(p) = plist_path(id) {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", p.display())),
            }
        }
        Ok(())
    } else if cfg!(windows) {
        let mut c = Command::new("schtasks.exe");
        c.args(["/Delete", "/TN", &task_name(id), "/F"]);
        hide(&mut c);
        let _ = c.output();
        Ok(())
    } else {
        Ok(())
    }
}

/// Whether the workflow's schedule entry is there and as approved:
/// `on`, `off` (no entry), `differs` (Repair), or `none` (no schedule).
fn schedule_state(v: &wf::WorkflowView, server: Option<&PathBuf>) -> &'static str {
    if v.at.is_none() {
        return "none";
    }
    if cfg!(target_os = "macos") {
        let Some(p) = plist_path(&v.id) else {
            return "off";
        };
        let Ok(text) = std::fs::read_to_string(&p) else {
            return "off";
        };
        // Same schedule and program as the workflow would get now.
        let expected = server.map(|s| {
            let times = launchd_plist(v, s, None);
            let cut = |t: &str| {
                t.split("<key>StartCalendarInterval</key>")
                    .nth(1)
                    .map(String::from)
            };
            cut(&times)
        });
        let have = text
            .split("<key>StartCalendarInterval</key>")
            .nth(1)
            .map(String::from);
        match expected {
            Some(e)
                if e == have
                    && server
                        .is_some_and(|s| text.contains(&xml_escape(&s.display().to_string()))) =>
            {
                "on"
            }
            _ => "differs",
        }
    } else if cfg!(windows) {
        let mut c = Command::new("schtasks.exe");
        c.args(["/Query", "/TN", &task_name(&v.id)]);
        hide(&mut c);
        match c.output() {
            Ok(o) if o.status.success() => "on",
            _ => "off",
        }
    } else {
        "off"
    }
}

#[allow(unused_variables)]
fn hide(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
}

#[cfg(unix)]
fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> wf::WorkflowView {
        let text = "id: crash-check\nname: Crash check\nschedule: { days: [mon, wed], at: \"06:30\" }\nscope: { connections: [c] }\nsteps:\n  - id: a\n    tool: query_features\n    args: { connection: c, dataset: d }\n";
        let w = wf::check(text).unwrap();
        wf::WorkflowView {
            id: w.id.clone(),
            name: w.name.clone(),
            purpose: String::new(),
            fingerprint: String::new(),
            plain: vec![],
            days: w.days().into_iter().map(String::from).collect(),
            at: Some("06:30".into()),
            tools: vec![],
            connections: vec![],
            state: None,
            problem: None,
            installed: None,
            proposal: None,
            text: None,
            replaces: None,
            last_run: None,
        }
    }

    /// The real scheduler takes the entry, and it can be removed again
    /// (run in CI on macOS and Windows; it writes the account's own
    /// scheduler, so it is not run by default).
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    #[ignore]
    fn the_systems_scheduler_takes_and_removes_an_entry() {
        let mut v = view();
        v.id = format!(
            "ci-check-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let server = std::env::current_exe().unwrap();
        schedule(&v, &server, None).expect("the scheduler takes the entry");
        assert_eq!(schedule_state(&v, Some(&server)), "on");
        unschedule(&v.id).unwrap();
        assert_eq!(schedule_state(&v, Some(&server)), "off");
        println!("the system's scheduler took and removed {}", v.id);
    }

    #[test]
    fn a_schedule_entry_names_only_the_workflow_and_its_engine() {
        let v = view();
        let p = launchd_plist(
            &v,
            Path::new("/Applications/Columbus.app/Contents/Resources/server/columbus-mcp"),
            Some(Path::new("/Users/me/Columbus & co/engine/venv/bin/python3")),
        );
        assert!(
            p.contains("<string>io.columbus.workflow.crash-check</string>"),
            "{p}"
        );
        assert!(p.contains("<string>run</string>\n    <string>crash-check</string>\n    <string>--scheduled</string>"), "{p}");
        assert!(p.contains("Columbus &amp; co"), "escaped: {p}");
        assert!(p.contains("<key>Weekday</key><integer>1</integer><key>Hour</key><integer>6</integer><key>Minute</key><integer>30</integer>"), "{p}");
        assert!(p.contains("<integer>3</integer>"), "Wednesday: {p}");
        assert!(
            !p.contains("<integer>5</integer><key>Hour"),
            "not Friday: {p}"
        );
        let t = task_xml(
            &v,
            Path::new(r"C:\Users\me\AppData\Local\Columbus\server\columbus-mcp.exe"),
            None,
            30,
        );
        assert!(t.contains("<Monday /><Wednesday />"), "{t}");
        assert!(t.contains("T06:30:00</StartBoundary>"), "{t}");
        assert!(t.contains("<Arguments>&quot;workflow&quot; &quot;run&quot; &quot;crash-check&quot; &quot;--scheduled&quot;</Arguments>"), "{t}");
        assert!(
            t.contains("<ExecutionTimeLimit>PT35M</ExecutionTimeLimit>"),
            "{t}"
        );
        assert!(
            t.contains("<StartWhenAvailable>true</StartWhenAvailable>"),
            "{t}"
        );
        assert!(t.contains("<LogonType>InteractiveToken</LogonType>"), "{t}");
        // With the windowless launcher beside the server, the task runs it.
        let dir = std::env::temp_dir().join(format!("wf-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let server = dir.join("columbus-mcp.exe");
        std::fs::write(&server, "").unwrap();
        std::fs::write(dir.join("columbus-workflow.exe"), "").unwrap();
        let t = task_xml(&v, &server, None, 30);
        assert!(t.contains("columbus-workflow.exe</Command>"), "{t}");
        assert!(
            t.contains("<Arguments>&quot;crash-check&quot; &quot;--scheduled&quot;</Arguments>"),
            "{t}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

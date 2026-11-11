//! A project's Sync section (build brief 3, D-20, REQ-43): link the
//! project's files folder to a repository on a connected Git server, check
//! for changes, sync, settle files that need a look, read the history,
//! disconnect.
//!
//! * What syncs is `projects/<id>/files/` (F-17), through the server's web
//!   API (`columbus-sync`): no Git program, nothing from a repository ever
//!   runs here.
//! * Nothing is sent unless the person presses Sync, and a sync sends only
//!   the changes the page showed them: what the last check found is kept
//!   here, and a file that changed after it waits for the next one.
//! * The link and the last synced state are kept in the app's data
//!   (`sync/<project>.json`, F-18), never in the files that sync, and hold
//!   no token.
//! * The project's shared settings travel as `columbus-project.yaml`. A
//!   change that arrives is shown (old and new) and applied only when the
//!   person accepts, through the project's normal save.
//! * The organization's policy is checked at every call (`allow_sync`,
//!   `sync.hosts`), in `git_servers`.
//! * A file that may hold data read from a source is held, not sent
//!   (R-103): the data labels the server keeps beside the project's files
//!   are read at every check and every sync, with the organization's flow
//!   rules. Such a file goes only after the person chose Send it anyway for
//!   it, as the last check showed it.
//!
//! The assistant has no part in this: nothing here is a tool.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

use columbus_shared::projects as pj;
use columbus_sync::settings::{self as shared, Shared};
use columbus_sync::{
    Changes, Choice, Commit, Conflict, DataLabels, Host, Keep, Kept, Link, Options, Preview,
    Record, Report,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

pub mod servers;

use servers as git_servers;

/// One check, sync or settling at a time: they read and write the same
/// folder and record.
static BUSY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What the last check of each project showed the person: the changes a
/// sync may send (path to what the file held), and the server's version of
/// each changed path (for the before and after).
#[derive(Default, Clone)]
struct Shown {
    seen: BTreeMap<String, String>,
    theirs: BTreeMap<String, String>,
    mine: Vec<String>,
    /// The files held because they may hold data, which the person may
    /// choose to send anyway: path to what the file held when it was shown.
    held: BTreeMap<String, String>,
}

static SHOWN: Mutex<Option<HashMap<String, Shown>>> = Mutex::new(None);

fn shown<R>(f: impl FnOnce(&mut HashMap<String, Shown>) -> R) -> R {
    let mut s = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    f(s.get_or_insert_with(HashMap::new))
}

fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|d| d.join("sync"))
        .map_err(|_| "this system gave the app no data folder".to_string())
}

/// Where a project's link and last synced state are kept.
fn record_path(app: &AppHandle, project: &str) -> Result<PathBuf, String> {
    Ok(data_dir(app)?.join(format!("{project}.json")))
}

/// Where files taken out of the folder go when the system's trash cannot
/// take them.
fn removed_dir(app: &AppHandle, project: &str) -> Result<PathBuf, String> {
    Ok(data_dir(app)?.join(format!("{project}-removed")))
}

/// Where the project's files folder is (F-17), whether or not it is there.
fn files_path(project: &str) -> PathBuf {
    pj::projects_dir().join(project).join("files")
}

/// The project's files folder (F-17), made if it is not there. Only what
/// links, checks or syncs makes it: looking at a project's page does not.
fn files_dir(project: &str) -> Result<PathBuf, String> {
    let folder = pj::projects_dir().join(project);
    if std::fs::symlink_metadata(&folder)
        .map(|m| !m.is_dir())
        .unwrap_or(true)
    {
        return Err("this project's folder is not there".into());
    }
    let files = folder.join("files");
    match std::fs::symlink_metadata(&files) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "{} is a link or a file, not a folder",
                files.display()
            ))
        }
        Err(_) => std::fs::create_dir(&files).map_err(|e| format!("{}: {e}", files.display()))?,
    }
    Ok(files)
}

/// A project's link and state, if it is linked. A record made for an
/// earlier project of the same id (one that was deleted since) is not this
/// project's: it is forgotten, and the project is not linked.
fn load_record(app: &AppHandle, p: &pj::Project) -> Result<Option<Record>, String> {
    let path = record_path(app, &p.id)?;
    let Some(record) = Record::load(&path).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if record.made_for.is_some() && record.made_for != p.created {
        Record::forget(&path).map_err(|e| e.to_string())?;
        shown(|s| s.remove(&p.id));
        return Ok(None);
    }
    Ok(Some(record))
}

fn save_record(app: &AppHandle, project: &str, record: &Record) -> Result<(), String> {
    record
        .save(&record_path(app, project)?)
        .map_err(|e| e.to_string())
}

fn project_settings(p: &pj::Project) -> Shared {
    Shared {
        name: p.name.clone(),
        instructions: p.instructions.clone(),
    }
}

#[derive(Serialize)]
struct ServerBrief {
    id: String,
    host: String,
    user: String,
    /// Why it cannot be used now, if so.
    problem: Option<String>,
}

#[derive(Serialize)]
struct LinkView {
    server: String,
    host: String,
    owner: String,
    repo: String,
    branch: String,
    /// Why the server cannot be used now, if so (removed, or no longer on
    /// the organization's list).
    problem: Option<String>,
}

/// An arriving change to the project's shared settings, waiting for the
/// person: what the project has, and what the file says.
#[derive(Serialize)]
struct SettingsChange {
    mine: Shared,
    theirs: Shared,
}

/// Everything the Sync section shows that needs no network.
#[derive(Serialize)]
pub struct SyncView {
    /// Whether the organization allows sync, and if not, the words to show.
    allowed: bool,
    why_not: Option<String>,
    servers: Vec<ServerBrief>,
    link: Option<LinkView>,
    synced_at: Option<String>,
    needs_look: Vec<Conflict>,
    settings: Option<SettingsChange>,
    /// The folder that syncs.
    folder: String,
}

fn view(app: &AppHandle, project: &str) -> Result<SyncView, String> {
    let p = pj::load(project)?;
    if let Err(why) = git_servers::allowed() {
        // Sync is off: nothing is offered, and nothing is listed.
        return Ok(SyncView {
            allowed: false,
            why_not: Some(why),
            servers: vec![],
            link: None,
            synced_at: None,
            needs_look: vec![],
            settings: None,
            folder: String::new(),
        });
    }
    let servers = git_servers::entries(app);
    let host_of = |url: &str, kind| {
        columbus_sync::remote::Address::parse(kind, url)
            .map(|a| a.host)
            .unwrap_or_default()
    };
    let briefs = servers
        .iter()
        .map(|e| {
            let host = host_of(&e.url, e.kind);
            ServerBrief {
                problem: git_servers::host_allowed(&host).err(),
                id: e.id.clone(),
                user: e.user.clone(),
                host,
            }
        })
        .collect();
    let folder = files_path(project);
    let mut record = load_record(app, &p)?;
    let mut settings = None;
    if let Some(r) = record.as_mut() {
        let mine = project_settings(&p);
        let before = r.settings.clone();
        if let Some(theirs) = shared::arriving(&folder, &mut r.settings, &mine) {
            settings = Some(SettingsChange { mine, theirs });
        }
        if r.settings != before {
            save_record(app, project, r)?;
        }
    }
    let link = record.as_ref().map(|r| {
        let server = servers.iter().find(|e| e.id == r.link.server);
        let host = server.map(|e| host_of(&e.url, e.kind)).unwrap_or_default();
        LinkView {
            problem: match server {
                None => Some("The Git server this project was linked to is no longer connected. Connect it again in Settings > Git servers, or disconnect the project.".into()),
                Some(_) => git_servers::host_allowed(&host).err(),
            },
            server: r.link.server.clone(),
            host,
            owner: r.link.owner.clone(),
            repo: r.link.repo.clone(),
            branch: r.link.branch.clone(),
        }
    });
    Ok(SyncView {
        allowed: true,
        why_not: None,
        servers: briefs,
        link,
        synced_at: record.as_ref().and_then(|r| r.state.synced_at.clone()),
        needs_look: record.map(|r| r.state.conflicts).unwrap_or_default(),
        settings,
        folder: folder.display().to_string(),
    })
}

/// The Sync section as it stands, without asking the server anything.
#[tauri::command]
pub async fn sync_state(app: AppHandle, project: String) -> Result<SyncView, String> {
    tauri::async_runtime::spawn_blocking(move || view(&app, &project))
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct RepoChoice {
    owner: String,
    name: String,
    private: bool,
    default_branch: String,
    /// Whether the token may send to it (else it can only be read).
    can_send: bool,
}

/// The repositories of the account, to pick one.
#[tauri::command]
pub async fn sync_repos(app: AppHandle, server: String) -> Result<Vec<RepoChoice>, String> {
    let (_, server) = git_servers::open(&app, &server)?;
    let repos = server.repos().await.map_err(|e| e.to_string())?;
    Ok(repos
        .into_iter()
        .filter(|r| !r.archived)
        .map(|r| RepoChoice {
            owner: r.owner,
            name: r.name,
            private: r.private,
            default_branch: r.default_branch,
            can_send: r.can_send,
        })
        .collect())
}

#[derive(Serialize)]
pub struct Branches {
    branches: Vec<String>,
    /// The branch the repository opens on (the one offered first).
    default: String,
}

/// A repository's branches, to choose the one to sync with.
#[tauri::command]
pub async fn sync_branches(
    app: AppHandle,
    server: String,
    owner: String,
    repo: String,
) -> Result<Branches, String> {
    let (_, server) = git_servers::open(&app, &server)?;
    let info = server
        .repo(&owner, &repo)
        .await
        .map_err(|e| e.to_string())?;
    let branches = server
        .branches(&owner, &repo)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Branches {
        branches,
        default: info.default_branch,
    })
}

#[derive(Deserialize)]
pub struct LinkForm {
    project: String,
    server: String,
    /// The repository's owner; empty for a new repository (the account's).
    #[serde(default)]
    owner: String,
    repo: String,
    /// Empty: the repository's default branch.
    #[serde(default)]
    branch: String,
    /// Make a new private repository of that name first.
    #[serde(default)]
    create: bool,
}

/// Link the project to a repository: one that exists, or a new private
/// one. Nothing of the project is sent by linking.
#[tauri::command]
pub async fn sync_link(app: AppHandle, form: LinkForm) -> Result<SyncView, String> {
    let _busy = BUSY.lock().await;
    let p = pj::load(&form.project)?;
    if load_record(&app, &p)?.is_some() {
        return Err("This project is linked already. Disconnect it first.".into());
    }
    let (entry, server) = git_servers::open(&app, &form.server)?;
    let info = if form.create {
        server
            .create_repo(form.repo.trim(), &format!("Columbus project: {}", p.name))
            .await
            .map_err(|e| e.to_string())?
    } else {
        server
            .repo(form.owner.trim(), form.repo.trim())
            .await
            .map_err(|e| e.to_string())?
    };
    let branch = match form.branch.trim() {
        "" => info.default_branch.clone(),
        b => b.to_string(),
    };
    // The branch must be there, unless the repository is empty (then its
    // first files make its default branch).
    let remote = server
        .remote(&info.owner, &info.name, &branch)
        .map_err(|e| e.to_string())?;
    remote.tip().await.map_err(|e| e.to_string())?;
    files_dir(&form.project)?;
    let mut record = Record::new(Link {
        server: entry.id,
        owner: info.owner,
        repo: info.name,
        branch,
    });
    // The link is this project's: a later project with the same id does
    // not take it over.
    record.made_for = p.created.clone();
    save_record(&app, &form.project, &record)?;
    shown(|s| s.remove(&form.project));
    view(&app, &form.project)
}

/// A linked project, ready to talk to its server.
struct Linked {
    record: Record,
    remote: columbus_sync::remote::Remote,
    folder: PathBuf,
    project: pj::Project,
    /// The data labels of the project's files, as they are now, with the
    /// organization's flow rules for this server.
    labels: DataLabels,
}

fn linked(app: &AppHandle, project: &str) -> Result<Linked, String> {
    let p = pj::load(project)?;
    git_servers::allowed()?;
    let record = load_record(app, &p)?.ok_or("This project is not linked to a repository.")?;
    let (entry, server) = git_servers::open(app, &record.link.server)?;
    let remote = server
        .remote(&record.link.owner, &record.link.repo, &record.link.branch)
        .map_err(|e| e.to_string())?;
    let host = columbus_sync::remote::Address::parse(entry.kind, &entry.url)
        .map(|a| a.host)
        .map_err(|e| e.to_string())?;
    let folder = files_dir(project)?;
    // The record is beside the files folder, in the project's own folder.
    let labels = DataLabels::load(
        &pj::projects_dir().join(project),
        &crate::settings::policy_now().flows,
        &host,
    );
    Ok(Linked {
        folder,
        record,
        remote,
        project: p,
        labels,
    })
}

#[derive(Serialize)]
pub struct Checked {
    view: SyncView,
    changes: Changes,
}

/// Compare the folder, the server and what was last synced. Only reads the
/// server; in the folder it only brings `columbus-project.yaml` up to date
/// with the project's own settings when the person changed them.
async fn check(app: &AppHandle, project: &str) -> Result<Changes, String> {
    let mut l = linked(app, project)?;
    let mine = project_settings(&l.project);
    let before = l.record.settings.clone();
    let mut changes = columbus_sync::check(&l.folder, &l.remote, &l.record.state, &l.labels)
        .await
        .map_err(|e| e.to_string())?;
    let on_server = changes
        .server
        .iter()
        .map(|i| &i.path)
        .chain(changes.both.iter().map(|i| &i.path))
        .any(|p| p == columbus_sync::paths::SETTINGS_FILE);
    let written = shared::prepare(&l.folder, &mut l.record.settings, &mine, on_server)
        .map_err(|e| e.to_string())?;
    if l.record.settings != before {
        save_record(app, project, &l.record)?;
    }
    if written {
        changes = columbus_sync::check(&l.folder, &l.remote, &l.record.state, &l.labels)
            .await
            .map_err(|e| e.to_string())?;
    }
    // What the person is about to be shown: only this may be sent.
    let mut s = Shown::default();
    for i in &changes.here {
        s.seen.insert(i.path.clone(), i.hash.clone());
        s.mine.push(i.path.clone());
    }
    for i in &changes.both {
        s.seen.insert(i.path.clone(), i.hash.clone());
        s.mine.push(i.path.clone());
        if !i.blob.is_empty() {
            s.theirs.insert(i.path.clone(), i.blob.clone());
        }
    }
    for i in &changes.server {
        if !i.blob.is_empty() {
            s.theirs.insert(i.path.clone(), i.blob.clone());
        }
        s.mine.push(i.path.clone());
    }
    // For a file changed here only, the server's version is the one last
    // synced.
    for i in &changes.here {
        if let Some(base) = l.record.state.files.get(&i.path) {
            s.theirs.insert(i.path.clone(), base.blob.clone());
        }
    }
    // Held files can be looked at too, and sent anyway only as shown here
    // (and only where the organization's rules leave that choice).
    for h in &changes.held {
        s.mine.push(h.path.clone());
        if let Some(base) = l.record.state.files.get(&h.path) {
            s.theirs.insert(h.path.clone(), base.blob.clone());
        }
        if h.choice {
            s.held.insert(h.path.clone(), h.hash.clone());
        }
    }
    shown(|all| all.insert(project.to_string(), s));
    Ok(changes)
}

/// Check for changes.
#[tauri::command]
pub async fn sync_check(app: AppHandle, project: String) -> Result<Checked, String> {
    let _busy = BUSY.lock().await;
    let changes = check(&app, &project).await?;
    Ok(Checked {
        view: view(&app, &project)?,
        changes,
    })
}

#[derive(Serialize)]
pub struct Synced {
    view: SyncView,
    report: Report,
    /// What is left after the sync (a fresh check).
    changes: Option<Changes>,
}

/// Sync: get the server's changes, keep both versions where both changed,
/// send what the person was shown as one change with their note.
#[tauri::command]
pub async fn sync_now(
    app: AppHandle,
    project: String,
    note: Option<String>,
) -> Result<Synced, String> {
    let _busy = BUSY.lock().await;
    let mut l = linked(&app, &project)?;
    let Some(seen) = shown(|s| s.get(&project).map(|x| x.seen.clone())) else {
        return Err("Check for changes first, so that you see what a sync would send.".into());
    };
    let keep = Keep::SystemTrash {
        fallback: removed_dir(&app, &project)?,
    };
    let today = columbus_sync::today();
    // The server's own token must never travel inside a file.
    let never_send: Vec<String> = git_servers::token_of(&l.record.link.server)
        .into_iter()
        .collect();
    let opts = Options {
        note: note.as_deref(),
        seen: Some(&seen),
        keep: &keep,
        today: &today,
        never_send: &never_send,
        labels: &l.labels,
    };
    let done = columbus_sync::sync(&l.folder, &l.remote, &mut l.record.state, &opts).await;
    // Whatever happened, what was done is recorded.
    save_record(&app, &project, &l.record)?;
    shown(|s| s.remove(&project));
    let report = done.map_err(|e| e.to_string())?;
    // A fresh look, so the page shows what is left (and what a next sync
    // would send).
    let changes = check(&app, &project).await.ok();
    Ok(Synced {
        view: view(&app, &project)?,
        report,
        changes,
    })
}

/// The person's choice for a file that is held because it may hold data
/// read from a source: **Send it anyway** (`send`), for the file exactly
/// as the last check showed it, or take that choice back. Nothing is sent
/// here: the file goes with the next sync, if it still holds the same.
/// Where the organization's flow rules forbid it, the file was not offered
/// the choice, and this refuses it.
#[tauri::command]
pub async fn sync_send_anyway(
    app: AppHandle,
    project: String,
    path: String,
    send: bool,
) -> Result<Checked, String> {
    let _busy = BUSY.lock().await;
    git_servers::allowed()?;
    let p = pj::load(&project)?;
    let mut record = load_record(&app, &p)?.ok_or("This project is not linked to a repository.")?;
    if send {
        let Some(hash) = shown(|s| s.get(&project).and_then(|x| x.held.get(&path).cloned())) else {
            return Err("That file is not among the held files the last check showed, or your organization's rules do not allow it to be sent. Check for changes again.".into());
        };
        record.state.send_anyway(&path, &hash);
    } else {
        record.state.hold_again(&path);
    }
    save_record(&app, &project, &record)?;
    // A fresh look: the file is now among what a sync sends, or held again.
    let changes = check(&app, &project).await?;
    Ok(Checked {
        view: view(&app, &project)?,
        changes,
    })
}

/// A file's two versions, for What changed: `mine` from the folder,
/// `theirs` from the server (the version the last check listed).
#[tauri::command]
pub async fn sync_preview(
    app: AppHandle,
    project: String,
    path: String,
) -> Result<Preview, String> {
    let l = linked(&app, &project)?;
    let Some(s) = shown(|s| s.get(&project).cloned()) else {
        return Err("Check for changes first.".into());
    };
    if !s.mine.contains(&path) {
        return Err("That file is not among the changes shown.".into());
    }
    columbus_sync::preview(
        &l.folder,
        &l.remote,
        Some(&path),
        s.theirs.get(&path).map(String::as_str),
    )
    .await
    .map_err(|e| e.to_string())
}

/// Both versions of a file that needs a look, from the folder: the
/// person's copy (`mine`) and the file that has the name (`theirs`).
#[tauri::command]
pub async fn sync_preview_kept(
    app: AppHandle,
    project: String,
    path: String,
    copy: Option<String>,
) -> Result<Preview, String> {
    let l = linked(&app, &project)?;
    if !l
        .record
        .state
        .conflicts
        .iter()
        .any(|c| c.path == path && c.copy == copy)
    {
        return Err("That file no longer needs a look.".into());
    }
    let mine = columbus_sync::preview(&l.folder, &l.remote, copy.as_deref(), None)
        .await
        .map_err(|e| e.to_string())?;
    let theirs = columbus_sync::preview(&l.folder, &l.remote, Some(&path), None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Preview {
        mine: mine.mine,
        theirs: theirs.mine,
    })
}

#[derive(Serialize)]
pub struct Resolved {
    view: SyncView,
    /// Where a file that was taken out of the folder is kept, if one was.
    kept: Option<Kept>,
}

/// Settle a file that needs a look: keep mine, keep theirs, or keep both.
/// Nothing is sent; what stays in the folder goes with the next sync.
#[tauri::command]
pub async fn sync_resolve(
    app: AppHandle,
    project: String,
    path: String,
    copy: Option<String>,
    choice: Choice,
) -> Result<Resolved, String> {
    let _busy = BUSY.lock().await;
    git_servers::allowed()?;
    let p = pj::load(&project)?;
    let mut record = load_record(&app, &p)?.ok_or("This project is not linked to a repository.")?;
    let folder = files_dir(&project)?;
    let keep = Keep::SystemTrash {
        fallback: removed_dir(&app, &project)?,
    };
    let app2 = app.clone();
    let project2 = project.clone();
    let kept = tauri::async_runtime::spawn_blocking(move || {
        let kept = columbus_sync::resolve(
            &folder,
            &mut record.state,
            &path,
            copy.as_deref(),
            choice,
            &keep,
        )
        .map_err(|e| e.to_string())?;
        save_record(&app2, &project2, &record)?;
        Ok::<_, String>(kept)
    })
    .await
    .map_err(|e| e.to_string())??;
    shown(|s| s.remove(&project));
    Ok(Resolved {
        view: view(&app, &project)?,
        kept,
    })
}

/// The repository's recent changes: who, when, and their note.
#[tauri::command]
pub async fn sync_history(app: AppHandle, project: String) -> Result<Vec<Commit>, String> {
    let l = linked(&app, &project)?;
    l.remote.history(30).await.map_err(|e| e.to_string())
}

/// Forget the link. The files stay as they are, here and on the server.
#[tauri::command]
pub async fn sync_disconnect(app: AppHandle, project: String) -> Result<SyncView, String> {
    let _busy = BUSY.lock().await;
    pj::load(&project)?;
    Record::forget(&record_path(&app, &project)?).map_err(|e| e.to_string())?;
    shown(|s| s.remove(&project));
    view(&app, &project)
}

/// The person's answer to an arriving change of the project's shared
/// settings: use them (the project is saved with them, through its normal
/// save and its checks), or keep the project's own.
#[tauri::command]
pub async fn sync_settings(
    app: AppHandle,
    project: String,
    accept: bool,
) -> Result<SyncView, String> {
    let _busy = BUSY.lock().await;
    git_servers::allowed()?;
    let p = pj::load(&project)?;
    let mut record = load_record(&app, &p)?.ok_or("This project is not linked to a repository.")?;
    let folder = files_dir(&project)?;
    let mine = project_settings(&p);
    let Some(theirs) = shared::arriving(&folder, &mut record.settings, &mine) else {
        return view(&app, &project);
    };
    if accept {
        // The project's own save: the same checks as a change typed into
        // the project's page.
        pj::save(&pj::Project {
            name: theirs.name.clone(),
            instructions: theirs.instructions.clone(),
            ..p
        })
        .map_err(|e| format!("These settings cannot be used: {e}"))?;
        shared::accepted(&mut record.settings, &theirs);
    } else {
        shared::declined(&mut record.settings, &theirs, &mine);
    }
    save_record(&app, &project, &record)?;
    view(&app, &project)
}

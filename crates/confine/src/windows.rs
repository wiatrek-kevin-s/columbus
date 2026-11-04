//! Windows confinement (design change 1, section 8).
//!
//! * Levels A and B: the program runs in an AppContainer named after the
//!   spec (`columbus.<name>`), with no capabilities (A) or the
//!   internetClient capability (B). An AppContainer can open only what is
//!   granted to its SID or to all application packages, so the read-write
//!   and read-only folders are granted to that SID by ACL; everything else
//!   in the user's profile is out of reach. AppContainers cannot reach
//!   loopback, which is why desktop-app servers need level C.
//! * Level C: the program runs with a restricted copy of the launcher's
//!   token (privileges removed) at low integrity. It can read what the user
//!   can, but write only where a low integrity label allows: the read-write
//!   folders, which are labelled low.
//! * All levels: the program is placed in a job object that kills it (and
//!   anything it started) when the launcher ends, with optional memory and
//!   process limits. The launcher stays alive as the gateway's child, so
//!   when the gateway stops it, the job closes and the program goes too.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::core::{HRESULT, PCWSTR, PWSTR};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, HLOCAL,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
    GetEffectiveRightsFromAclW, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW,
    EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, TRUSTEE_IS_SID,
    TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows_sys::Win32::Security::{
    AddMandatoryAce, CreateRestrictedToken, CreateWellKnownSid, FreeSid, GetLengthSid,
    InitializeAcl, SetTokenInformation, TokenIntegrityLevel, WinCapabilityInternetClientSid, ACL,
    ACL_REVISION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE,
    LABEL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::SystemServices::{
    SE_GROUP_INTEGRITY, SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, CreateProcessAsUserW, CreateProcessW, DeleteProcThreadAttributeList,
    GetCurrentProcess, GetExitCodeProcess, InitializeProcThreadAttributeList, OpenProcessToken,
    ReleaseMutex, ResumeThread, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW,
};

use crate::{Error, Level, Spec};

/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`
const HR_ALREADY_EXISTS: HRESULT = 0x800700B7u32 as HRESULT;
/// The low mandatory integrity level.
const LOW_INTEGRITY_SID: &str = "S-1-16-4096";

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> Error {
    Error::Setup(format!("{what}: {}", std::io::Error::last_os_error()))
}

/// Closes a handle when dropped.
struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A SID allocated by Windows, freed the way it was allocated.
enum Sid {
    Free(PSID),
    Local(PSID),
}
impl Sid {
    fn ptr(&self) -> PSID {
        match self {
            Sid::Free(p) | Sid::Local(p) => *p,
        }
    }
}
impl Drop for Sid {
    fn drop(&mut self) {
        unsafe {
            match self {
                Sid::Free(p) => {
                    FreeSid(*p);
                }
                Sid::Local(p) => {
                    LocalFree(*p as HLOCAL);
                }
            }
        }
    }
}

fn string_sid(s: &str) -> Result<Sid, Error> {
    let w = wide(s);
    let mut sid: PSID = null_mut();
    if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut sid) } == 0 {
        return Err(last_error("ConvertStringSidToSid"));
    }
    Ok(Sid::Local(sid))
}

/// Held while one launcher sets up confinement, so launchers starting at
/// the same time do not race: two creating the same AppContainer profile
/// get E_UNEXPECTED, or one starts its process before the other has
/// finished registering the profile; and folder grants rewrite ACLs.
///
/// Best effort. The mutex is labelled so that nothing below medium
/// integrity (a level C process runs as the same user at low integrity) can
/// wait on it, hold it or signal it. If it cannot be had within 30 seconds
/// (another launcher granting a very large folder, or a confined process
/// that created the name first), setup goes ahead without it and retries
/// the races it guards against, rather than letting a confined process
/// block every later launch.
struct SetupLock(Option<HANDLE>);

impl SetupLock {
    fn take() -> Self {
        let name = wide("Local\\columbus-confine-setup");
        // Full access for the owner and SYSTEM; no read, write or execute
        // (SYNCHRONIZE is execute for a mutex) from below medium integrity.
        let sddl = wide("D:P(A;;GA;;;OW)(A;;GA;;;SY)S:(ML;;NWNRNX;;;ME)");
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        let have_sd = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut sd,
                null_mut(),
            )
        } != 0;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let h = unsafe { CreateMutexW(if have_sd { &sa } else { null() }, 0, name.as_ptr()) };
        if !sd.is_null() {
            unsafe { LocalFree(sd as HLOCAL) };
        }
        if h.is_null() {
            return SetupLock(None);
        }
        // WAIT_OBJECT_0 or WAIT_ABANDONED (a launcher that died holding it):
        // either way this launcher now owns it.
        let r = unsafe { WaitForSingleObject(h, 30_000) };
        if r == 0 || r == 0x80 {
            SetupLock(Some(h))
        } else {
            unsafe { CloseHandle(h) };
            SetupLock(None)
        }
    }
}

impl Drop for SetupLock {
    fn drop(&mut self) {
        if let Some(h) = self.0 {
            unsafe {
                ReleaseMutex(h);
                CloseHandle(h);
            }
        }
    }
}

/// `E_UNEXPECTED`: what CreateAppContainerProfile returns when another
/// process is creating the same profile at that moment.
const HR_UNEXPECTED: HRESULT = 0x8000FFFFu32 as HRESULT;

/// The AppContainer for this spec, created on first use.
fn app_container(name: &str) -> Result<Sid, Error> {
    let full = format!("columbus.{name}");
    let wname = wide(&full);
    let display = wide(format!("Columbus {name}"));
    let desc = wide("A Columbus local server or engine plugin, confined");
    let mut sid: PSID = null_mut();
    let mut hr = HR_UNEXPECTED;
    for attempt in 0..20 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        hr = unsafe {
            CreateAppContainerProfile(
                wname.as_ptr(),
                display.as_ptr(),
                desc.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if hr != HR_UNEXPECTED {
            break;
        }
    }
    if hr == HR_ALREADY_EXISTS {
        let hr = unsafe { DeriveAppContainerSidFromAppContainerName(wname.as_ptr(), &mut sid) };
        if hr < 0 {
            return Err(Error::Setup(format!(
                "DeriveAppContainerSidFromAppContainerName: HRESULT {hr:#x}"
            )));
        }
    } else if hr < 0 {
        return Err(Error::Setup(format!(
            "CreateAppContainerProfile: HRESULT {hr:#x}"
        )));
    }
    Ok(Sid::Free(sid))
}

fn trustee(sid: PSID) -> TRUSTEE_W {
    TRUSTEE_W {
        pMultipleTrustee: null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
        ptstrName: sid as PWSTR,
    }
}

/// Grant `access` on `path` (and, for a folder, everything in it) to `sid`,
/// unless it already has it: re-applying an inheritable ACE walks the whole
/// tree, which is slow on an interpreter's folder.
fn grant(path: &Path, sid: PSID, access: u32) -> Result<(), Error> {
    let wpath = wide(path);
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let r = unsafe {
        GetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if r != 0 {
        return Err(Error::Setup(format!(
            "reading the permissions of {}: error {r}",
            path.display()
        )));
    }
    let t = trustee(sid);
    let mut have: u32 = 0;
    let known = unsafe { GetEffectiveRightsFromAclW(dacl, &t, &mut have) } == 0;
    let result = if known && have & access == access {
        Ok(())
    } else {
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: access,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: if path.is_dir() {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                0
            },
            Trustee: t,
        };
        let mut new_dacl: *mut ACL = null_mut();
        let r = unsafe { SetEntriesInAclW(1, &ea, dacl, &mut new_dacl) };
        if r != 0 {
            Err(Error::Setup(format!("SetEntriesInAcl: error {r}")))
        } else {
            let r = unsafe {
                SetNamedSecurityInfoW(
                    wpath.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    new_dacl,
                    null(),
                )
            };
            unsafe { LocalFree(new_dacl as HLOCAL) };
            if r != 0 {
                Err(Error::Setup(format!(
                    "granting access to {}: error {r}",
                    path.display()
                )))
            } else {
                Ok(())
            }
        }
    };
    unsafe { LocalFree(sd as HLOCAL) };
    result
}

/// Label `path` (inherited by everything in it) low integrity, so a level C
/// process may write there and nowhere else.
fn label_low(path: &Path, low: PSID) -> Result<(), Error> {
    let len = 8 + 16 + unsafe { GetLengthSid(low) } as usize;
    // ACLs must be 4-byte aligned.
    let mut buf = vec![0u32; len.div_ceil(4)];
    let acl = buf.as_mut_ptr() as *mut ACL;
    unsafe {
        if InitializeAcl(acl, (buf.len() * 4) as u32, ACL_REVISION) == 0 {
            return Err(last_error("InitializeAcl"));
        }
        if AddMandatoryAce(
            acl,
            ACL_REVISION,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
            low,
        ) == 0
        {
            return Err(last_error("AddMandatoryAce"));
        }
    }
    let wpath = wide(path);
    let r = unsafe {
        SetNamedSecurityInfoW(
            wpath.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null(),
            acl,
        )
    };
    if r != 0 {
        return Err(Error::Setup(format!(
            "labelling {} low integrity: error {r}",
            path.display()
        )));
    }
    Ok(())
}

/// A restricted copy of this process's token at low integrity, for level C.
fn low_token(low: PSID) -> Result<Owned, Error> {
    let mut tok: HANDLE = null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
            &mut tok,
        )
    } == 0
    {
        return Err(last_error("OpenProcessToken"));
    }
    let tok = Owned(tok);
    let mut restricted: HANDLE = null_mut();
    if unsafe {
        CreateRestrictedToken(
            tok.0,
            DISABLE_MAX_PRIVILEGE,
            0,
            null(),
            0,
            null(),
            0,
            null(),
            &mut restricted,
        )
    } == 0
    {
        return Err(last_error("CreateRestrictedToken"));
    }
    let restricted = Owned(restricted);
    let tml = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: low,
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    let size = std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(low) };
    if unsafe {
        SetTokenInformation(
            restricted.0,
            TokenIntegrityLevel,
            &tml as *const _ as *const core::ffi::c_void,
            size,
        )
    } == 0
    {
        return Err(last_error("setting low integrity"));
    }
    Ok(restricted)
}

/// Quote one argument the way `CommandLineToArgvW` and the C runtime read it.
fn quote_arg(a: &str, out: &mut String) {
    if !a.is_empty() && !a.contains([' ', '\t', '\n', '\x0b', '"']) {
        out.push_str(a);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// The program's full path: as given when it has a folder, else found on
/// PATH (with `.exe` added when it has no extension).
fn resolve_program(program: &str) -> Result<PathBuf, Error> {
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let names: Vec<String> = if p.extension().is_some() {
        vec![program.to_string()]
    } else {
        vec![format!("{program}.exe"), program.to_string()]
    };
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for n in &names {
            let c = dir.join(n);
            if c.is_file() {
                return Ok(c);
            }
        }
    }
    Err(Error::Setup(format!("{program} was not found on PATH")))
}

/// The three standard handles, inheritable, for the child.
fn std_handles() -> Result<[HANDLE; 3], Error> {
    let mut hs = [null_mut(); 3];
    for (i, which) in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .enumerate()
    {
        let h = unsafe { GetStdHandle(which) };
        if !h.is_null() && h != INVALID_HANDLE_VALUE {
            unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        }
        hs[i] = h;
    }
    Ok(hs)
}

/// An initialized attribute list, deleted when dropped.
struct Attributes {
    buf: Vec<u8>,
}
impl Attributes {
    fn new(count: u32) -> Result<Self, Error> {
        let mut size = 0usize;
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        let mut buf = vec![0u8; size];
        if unsafe {
            InitializeProcThreadAttributeList(
                buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST,
                count,
                0,
                &mut size,
            )
        } == 0
        {
            return Err(last_error("InitializeProcThreadAttributeList"));
        }
        Ok(Attributes { buf })
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }
    fn set<T>(&mut self, attribute: u32, value: *const T, size: usize) -> Result<(), Error> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.ptr(),
                0,
                attribute as usize,
                value as *const core::ffi::c_void,
                size,
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(last_error("UpdateProcThreadAttribute"));
        }
        Ok(())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn job(spec: &Spec) -> Result<Owned, Error> {
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(last_error("CreateJobObject"));
    }
    let job = Owned(job);
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    if let Some(mb) = spec.memory_mb {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        info.JobMemoryLimit = (mb as usize).saturating_mul(1024 * 1024);
    }
    if let Some(n) = spec.processes {
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        info.BasicLimitInformation.ActiveProcessLimit = n.max(1);
    }
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(last_error("SetInformationJobObject"));
    }
    Ok(job)
}

pub fn launch(spec: &Spec, program: &str, args: &[String]) -> Result<i32, Error> {
    let exe = resolve_program(program)?;
    let mut cmdline = String::new();
    quote_arg(&exe.display().to_string(), &mut cmdline);
    for a in args {
        cmdline.push(' ');
        quote_arg(a, &mut cmdline);
    }
    let mut wcmd = wide(&cmdline);
    let wexe = wide(&exe);
    // The program's own folder is always readable.
    let mut read_only: Vec<PathBuf> = spec.read_only.clone();
    if let Some(dir) = exe.parent() {
        read_only.push(dir.to_path_buf());
    }

    let handles = std_handles()?;
    let inheritable: Vec<HANDLE> = {
        let mut v: Vec<HANDLE> = Vec::new();
        for h in handles {
            if !h.is_null() && h != INVALID_HANDLE_VALUE && !v.contains(&h) {
                v.push(h);
            }
        }
        v
    };
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = handles[0];
    si.StartupInfo.hStdOutput = handles[1];
    si.StartupInfo.hStdError = handles[2];
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT;
    let job = job(spec)?;
    let setup = SetupLock::take();

    // Kept alive until the process has been created.
    let mut _capability_sid = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let _container;
    let _low;
    let mut caps_list: Vec<SID_AND_ATTRIBUTES> = Vec::new();
    let mut security: SECURITY_CAPABILITIES = unsafe { std::mem::zeroed() };
    let created = match spec.level {
        Level::A | Level::B => {
            let container = app_container(&spec.name)?;
            for p in &spec.read_write {
                grant(p, container.ptr(), FILE_ALL_ACCESS)?;
            }
            for p in &read_only {
                if p.exists() {
                    grant(p, container.ptr(), FILE_GENERIC_READ | FILE_GENERIC_EXECUTE)?;
                }
            }
            if spec.level == Level::B {
                let mut size = SECURITY_MAX_SID_SIZE;
                if unsafe {
                    CreateWellKnownSid(
                        WinCapabilityInternetClientSid,
                        null_mut(),
                        _capability_sid.as_mut_ptr() as PSID,
                        &mut size,
                    )
                } == 0
                {
                    return Err(last_error("CreateWellKnownSid(internetClient)"));
                }
                caps_list.push(SID_AND_ATTRIBUTES {
                    Sid: _capability_sid.as_mut_ptr() as PSID,
                    Attributes: 4, // SE_GROUP_ENABLED
                });
            }
            security.AppContainerSid = container.ptr();
            security.Capabilities = if caps_list.is_empty() {
                null_mut()
            } else {
                caps_list.as_mut_ptr()
            };
            security.CapabilityCount = caps_list.len() as u32;
            let mut attrs = Attributes::new(2)?;
            attrs.set(
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                &security,
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
            )?;
            attrs.set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                inheritable.as_ptr(),
                inheritable.len() * std::mem::size_of::<HANDLE>(),
            )?;
            si.lpAttributeList = attrs.ptr();
            _container = container;
            let ok = unsafe {
                CreateProcessW(
                    wexe.as_ptr(),
                    wcmd.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    flags,
                    null(),
                    null() as PCWSTR,
                    &si as *const STARTUPINFOEXW as *const STARTUPINFOW,
                    &mut pi,
                )
            };
            // Before anything else can overwrite the thread's last error.
            let e = (ok == 0).then(|| last_error(&format!("starting {}", exe.display())));
            drop(attrs);
            e
        }
        Level::C => {
            let low = string_sid(LOW_INTEGRITY_SID)?;
            for p in &spec.read_write {
                label_low(p, low.ptr())?;
            }
            let token = low_token(low.ptr())?;
            let mut attrs = Attributes::new(1)?;
            attrs.set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                inheritable.as_ptr(),
                inheritable.len() * std::mem::size_of::<HANDLE>(),
            )?;
            si.lpAttributeList = attrs.ptr();
            let ok = unsafe {
                CreateProcessAsUserW(
                    token.0,
                    wexe.as_ptr(),
                    wcmd.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    flags,
                    null(),
                    null() as PCWSTR,
                    &si as *const STARTUPINFOEXW as *const STARTUPINFOW,
                    &mut pi,
                )
            };
            let e = (ok == 0).then(|| last_error(&format!("starting {}", exe.display())));
            drop(attrs);
            _low = low;
            e
        }
    };
    drop(setup);
    if let Some(e) = created {
        return Err(e);
    }
    let process = Owned(pi.hProcess);
    let thread = Owned(pi.hThread);
    if unsafe { AssignProcessToJobObject(job.0, process.0) } == 0 {
        let e = last_error("AssignProcessToJobObject");
        // Never let it run outside the job.
        unsafe { windows_sys::Win32::System::Threading::TerminateProcess(process.0, 1) };
        return Err(e);
    }
    unsafe { ResumeThread(thread.0) };
    drop(thread);
    unsafe { WaitForSingleObject(process.0, INFINITE) };
    let mut code: u32 = 1;
    unsafe { GetExitCodeProcess(process.0, &mut code) };
    drop(job);
    Ok(code as i32)
}

#[cfg(test)]
mod tests {
    use super::quote_arg;

    #[test]
    fn quoting_matches_the_c_runtime() {
        let q = |a: &str| {
            let mut s = String::new();
            quote_arg(a, &mut s);
            s
        };
        assert_eq!(q("plain"), "plain");
        assert_eq!(q("two words"), "\"two words\"");
        assert_eq!(q(""), "\"\"");
        assert_eq!(q("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(q("C:\\dir with space\\"), "\"C:\\dir with space\\\\\"");
    }
}

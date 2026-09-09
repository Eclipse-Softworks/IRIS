//! Windows AppContainer + Job Object worker launcher.

use super::{
    CommandSpec, LimitViolation, ResourceLimits, SandboxAvailability, SandboxError,
    SandboxRunOutput,
};
use std::ffi::{c_void, OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, GetAppContainerFolderPath,
};
use windows_sys::Win32::Security::{
    FreeSid, GetTokenInformation, TokenIsAppContainer, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
    TOKEN_QUERY,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_JOB_TIME, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, ResumeThread, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};
use windows_sys::Win32::System::WindowsProgramming::PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;

static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(0);
const POLL_INTERVAL_MS: u32 = 5;
const TERMINATED_BY_SUPERVISOR: u32 = 0xE175_0001;

pub(super) fn detect() -> SandboxAvailability {
    SandboxAvailability::Enforced {
        capability_isolation: "Windows LPAC (zero capabilities)",
        resource_limits: "Windows Job Object + bounded supervisor pipes",
    }
}

pub(super) fn current_process_is_appcontainer() -> Result<bool, SandboxError> {
    let mut token = null_mut();
    // SAFETY: the pseudo process handle is valid and `token` is an out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error(
            "open process token for AppContainer attestation",
        ));
    }
    let token = OwnedHandle::new(token)
        .ok_or_else(|| SandboxError::Platform("OpenProcessToken returned null".into()))?;
    let mut is_appcontainer = 0_u32;
    let mut returned = 0_u32;
    // SAFETY: the token is live and the output buffer matches the information
    // class documented for TokenIsAppContainer.
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenIsAppContainer,
            &mut is_appcontainer as *mut u32 as *mut c_void,
            u32::try_from(size_of::<u32>()).expect("u32 size fits in u32"),
            &mut returned,
        )
    } == 0
    {
        Err(last_error("query AppContainer process token"))
    } else {
        Ok(is_appcontainer != 0)
    }
}

pub(super) fn run(
    command: &CommandSpec,
    limits: ResourceLimits,
) -> Result<SandboxRunOutput, SandboxError> {
    limits.validate().map_err(SandboxError::InvalidLimits)?;
    let source_executable = command
        .executable()
        .canonicalize()
        .map_err(|error| platform_error("canonicalize worker executable", error))?;
    if !source_executable.is_file() {
        return Err(SandboxError::InvalidCommand(
            "worker executable is not a regular file".into(),
        ));
    }

    let profile = AppContainerProfile::create()?;
    let staged_executable = profile.folder.join("iris-sandbox-worker.exe");
    fs::copy(&source_executable, &staged_executable)
        .map_err(|error| platform_error("stage worker in AppContainer profile", error))?;

    let job = Job::create(limits)?;
    let stdin = inheritable_null_input()?;
    let (stdout_read, stdout_write) = pipe()?;
    let (stderr_read, stderr_write) = pipe()?;
    let inherited_handles = [stdin.raw(), stdout_write.raw(), stderr_write.raw()];

    let security_capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid.0,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let all_application_packages_policy = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
    let mut attributes = AttributeList::new(3)?;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
        &security_capabilities as *const SECURITY_CAPABILITIES as *const c_void,
        size_of::<SECURITY_CAPABILITIES>(),
    )?;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited_handles.as_ptr() as *const c_void,
        size_of_val(&inherited_handles),
    )?;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY as usize,
        &all_application_packages_policy as *const u32 as *const c_void,
        size_of::<u32>(),
    )?;

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb =
        u32::try_from(size_of::<STARTUPINFOEXW>()).expect("STARTUPINFOEXW size fits in u32");
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.raw();
    startup.StartupInfo.hStdOutput = stdout_write.raw();
    startup.StartupInfo.hStdError = stderr_write.raw();
    startup.lpAttributeList = attributes.as_ptr();

    let application = wide_null(staged_executable.as_os_str());
    let mut command_line = build_command_line(&staged_executable, command.arguments());
    let current_directory = wide_null(profile.folder.as_os_str());
    let environment = minimal_environment(&profile.folder);
    let mut process_info = PROCESS_INFORMATION::default();
    // SAFETY: all pointers refer to live, correctly initialized buffers. The
    // handle list contains only inheritable handles owned by this function.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_ptr() as *const c_void,
            current_directory.as_ptr(),
            &startup.StartupInfo,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(last_error("create AppContainer worker process"));
    }
    let mut process = Process::new(process_info);

    // Parent copies of the write ends must be closed before readers start so
    // EOF is observable when the sandboxed process tree exits.
    drop(stdout_write);
    drop(stderr_write);
    drop(stdin);

    job.assign(process.process())?;

    let total_output = Arc::new(AtomicU64::new(0));
    let output_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_reader(
        stdout_read.into_file(),
        Arc::clone(&total_output),
        Arc::clone(&output_exceeded),
        limits.output_bytes,
    );
    let stderr_reader = spawn_reader(
        stderr_read.into_file(),
        Arc::clone(&total_output),
        Arc::clone(&output_exceeded),
        limits.output_bytes,
    );

    // The process has not executed any candidate instruction before this
    // point: it was born suspended and is already attached to the Job.
    // SAFETY: the primary thread handle is live until `resume` returns.
    let resume_result = unsafe { ResumeThread(process.thread()) };
    if resume_result == u32::MAX {
        return Err(last_error("resume sandbox worker"));
    }
    process.close_thread();

    let started = Instant::now();
    let mut violation = None;
    loop {
        // SAFETY: the process handle stays live for the duration of the loop.
        match unsafe { WaitForSingleObject(process.process(), POLL_INTERVAL_MS) } {
            WAIT_OBJECT_0 => break,
            WAIT_TIMEOUT => {}
            other => {
                job.terminate(TERMINATED_BY_SUPERVISOR);
                return Err(SandboxError::Platform(format!(
                    "wait for sandbox worker returned unexpected status {other}"
                )));
            }
        }

        let elapsed_ms = millis(started.elapsed());
        if elapsed_ms > limits.wall_time_ms {
            violation = Some(LimitViolation::WallTime {
                elapsed_ms,
                limit_ms: limits.wall_time_ms,
            });
        } else if output_exceeded.load(Ordering::Acquire) {
            violation = Some(LimitViolation::Output {
                observed_bytes: total_output.load(Ordering::Acquire),
                limit_bytes: limits.output_bytes,
            });
        } else {
            let cpu_time_ms = job.cpu_time_ms()?;
            if cpu_time_ms > limits.cpu_time_ms {
                violation = Some(LimitViolation::CpuTime {
                    observed_ms: cpu_time_ms,
                    limit_ms: limits.cpu_time_ms,
                });
            }
        }

        if violation.is_some() {
            job.terminate(TERMINATED_BY_SUPERVISOR);
            // SAFETY: termination is asynchronous; wait for handle teardown so
            // inherited pipe handles are closed before joining readers.
            unsafe { WaitForSingleObject(process.process(), 5_000) };
            break;
        }
    }

    let exit_code = process.exit_code()?;
    process.mark_exited();
    let cpu_time_ms = job.cpu_time_ms()?;
    let peak_memory_bytes = job.peak_memory_bytes()?;
    // The primary process may have exited while descendants still hold pipe
    // handles. Terminating the Job closes the complete process tree.
    job.terminate(exit_code);

    let stdout = join_reader(stdout_reader, "stdout")?;
    let stderr = join_reader(stderr_reader, "stderr")?;
    let output_bytes = total_output.load(Ordering::Acquire);
    if let Some(violation) = violation {
        return Err(SandboxError::LimitExceeded(violation));
    }
    if output_exceeded.load(Ordering::Acquire) {
        return Err(SandboxError::LimitExceeded(LimitViolation::Output {
            observed_bytes: output_bytes,
            limit_bytes: limits.output_bytes,
        }));
    }

    Ok(SandboxRunOutput {
        exit_code,
        stdout,
        stderr,
        wall_time_ms: millis(started.elapsed()),
        cpu_time_ms,
        peak_memory_bytes,
        output_bytes,
    })
}

struct AppContainerProfile {
    name: Vec<u16>,
    sid: Sid,
    folder: PathBuf,
}

impl AppContainerProfile {
    fn create() -> Result<Self, SandboxError> {
        let unique = PROFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        let name = format!(
            "IRIS.NativeSandbox.{}.{}.{}",
            std::process::id(),
            nanos,
            unique
        );
        let name = wide_null(OsStr::new(&name));
        let display = wide_null(OsStr::new("IRIS native validation worker"));
        let description = wide_null(OsStr::new("Ephemeral zero-capability IRIS sandbox"));
        let mut sid = null_mut();
        // SAFETY: all strings are NUL terminated and `sid` is a valid out pointer.
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if result < 0 || sid.is_null() {
            return Err(SandboxError::Platform(format!(
                "create AppContainer profile failed with HRESULT 0x{:08X}",
                result as u32
            )));
        }
        let sid = Sid(sid);
        let folder = match appcontainer_folder(sid.0) {
            Ok(folder) => folder,
            Err(error) => {
                delete_profile(&name);
                return Err(error);
            }
        };
        if let Err(error) = fs::create_dir_all(&folder) {
            delete_profile(&name);
            return Err(platform_error("create AppContainer data folder", error));
        }
        Ok(Self { name, sid, folder })
    }
}

impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        delete_profile(&self.name);
    }
}

fn delete_profile(name: &[u16]) {
    // SAFETY: the profile name is NUL terminated. Failure during best-effort
    // cleanup cannot weaken a completed or rejected run.
    unsafe { DeleteAppContainerProfile(name.as_ptr()) };
}

struct Sid(windows_sys::Win32::Security::PSID);

impl Drop for Sid {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: AppContainer profile APIs return a SID freed by FreeSid.
            unsafe { FreeSid(self.0) };
        }
    }
}

fn appcontainer_folder(sid: windows_sys::Win32::Security::PSID) -> Result<PathBuf, SandboxError> {
    let mut sid_string = null_mut();
    // SAFETY: `sid` is live and `sid_string` is a valid out pointer.
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_string) } == 0 {
        return Err(last_error("convert AppContainer SID to string"));
    }
    let mut folder = null_mut();
    // SAFETY: `sid_string` is a NUL-terminated SID string allocated by Windows.
    let result = unsafe { GetAppContainerFolderPath(sid_string, &mut folder) };
    // SAFETY: ConvertSidToStringSidW allocates with LocalAlloc.
    unsafe { LocalFree(sid_string as *mut c_void) };
    if result < 0 || folder.is_null() {
        return Err(SandboxError::Platform(format!(
            "get AppContainer folder failed with HRESULT 0x{:08X}",
            result as u32
        )));
    }
    let path = PathBuf::from(wide_ptr_to_os_string(folder));
    // SAFETY: GetAppContainerFolderPath allocates with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(folder as *const c_void) };
    Ok(path)
}

struct Job(OwnedHandle);

impl Job {
    fn create(limits: ResourceLimits) -> Result<Self, SandboxError> {
        // SAFETY: null security attributes and name request an anonymous Job.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        let handle =
            OwnedHandle::new(handle).ok_or_else(|| last_error("create sandbox Job Object"))?;
        let memory_limit = usize::try_from(limits.memory_bytes).map_err(|_| {
            SandboxError::InvalidLimits("memory limit does not fit this platform".into())
        })?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_JOB_TIME
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        info.BasicLimitInformation.ActiveProcessLimit = limits.process_count;
        info.BasicLimitInformation.PerJobUserTimeLimit =
            i64::try_from(limits.cpu_time_ms.saturating_mul(10_000)).unwrap_or(i64::MAX);
        info.JobMemoryLimit = memory_limit;
        // SAFETY: `info` is the documented structure for this information class.
        let result = unsafe {
            SetInformationJobObject(
                handle.raw(),
                JobObjectExtendedLimitInformation,
                &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const c_void,
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .expect("Job information size fits in u32"),
            )
        };
        if result == 0 {
            return Err(last_error("apply sandbox Job Object limits"));
        }
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        };
        // SAFETY: `ui` is the documented structure for UI restrictions.
        let result = unsafe {
            SetInformationJobObject(
                handle.raw(),
                JobObjectBasicUIRestrictions,
                &ui as *const JOBOBJECT_BASIC_UI_RESTRICTIONS as *const c_void,
                u32::try_from(size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>())
                    .expect("Job UI information size fits in u32"),
            )
        };
        if result == 0 {
            return Err(last_error("apply sandbox Job UI restrictions"));
        }
        Ok(Self(handle))
    }

    fn assign(&self, process: HANDLE) -> Result<(), SandboxError> {
        // SAFETY: both handles are live and the worker is still suspended.
        if unsafe { AssignProcessToJobObject(self.0.raw(), process) } == 0 {
            Err(last_error("assign suspended worker to sandbox Job"))
        } else {
            Ok(())
        }
    }

    fn terminate(&self, exit_code: u32) {
        // SAFETY: the Job handle is live. Repeated termination is harmless.
        unsafe { TerminateJobObject(self.0.raw(), exit_code) };
    }

    fn accounting(&self) -> Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, SandboxError> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `info` matches the requested information class.
        let result = unsafe {
            QueryInformationJobObject(
                self.0.raw(),
                JobObjectBasicAccountingInformation,
                &mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION as *mut c_void,
                u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
                    .expect("Job accounting size fits in u32"),
                null_mut(),
            )
        };
        if result == 0 {
            Err(last_error("query sandbox Job accounting"))
        } else {
            Ok(info)
        }
    }

    fn cpu_time_ms(&self) -> Result<u64, SandboxError> {
        let info = self.accounting()?;
        let ticks = info
            .TotalUserTime
            .saturating_add(info.TotalKernelTime)
            .max(0) as u64;
        Ok(ticks / 10_000)
    }

    fn peak_memory_bytes(&self) -> Result<u64, SandboxError> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: `info` matches the requested information class.
        let result = unsafe {
            QueryInformationJobObject(
                self.0.raw(),
                JobObjectExtendedLimitInformation,
                &mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *mut c_void,
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .expect("Job information size fits in u32"),
                null_mut(),
            )
        };
        if result == 0 {
            Err(last_error("query sandbox Job memory"))
        } else {
            Ok(u64::try_from(info.PeakJobMemoryUsed).unwrap_or(u64::MAX))
        }
    }
}

struct AttributeList {
    storage: Vec<usize>,
}

impl AttributeList {
    fn new(count: u32) -> Result<Self, SandboxError> {
        let mut bytes = 0_usize;
        // SAFETY: the documented sizing call uses a null list.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes) };
        if bytes == 0 {
            return Err(last_error("size process attribute list"));
        }
        let words = bytes.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; words];
        // SAFETY: storage is pointer-aligned and at least `bytes` bytes long.
        if unsafe {
            InitializeProcThreadAttributeList(
                storage.as_mut_ptr() as *mut c_void,
                count,
                0,
                &mut bytes,
            )
        } == 0
        {
            return Err(last_error("initialize process attribute list"));
        }
        Ok(Self { storage })
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr() as *mut c_void
    }

    fn update(
        &mut self,
        attribute: usize,
        value: *const c_void,
        size: usize,
    ) -> Result<(), SandboxError> {
        // SAFETY: the list is initialized and `value` remains live through
        // CreateProcessW.
        if unsafe {
            UpdateProcThreadAttribute(self.as_ptr(), 0, attribute, value, size, null_mut(), null())
        } == 0
        {
            Err(last_error("update process attribute list"))
        } else {
            Ok(())
        }
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: the list was successfully initialized in `new`.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

struct Process {
    process: OwnedHandle,
    thread: Option<OwnedHandle>,
    exited: bool,
}

impl Process {
    fn new(info: PROCESS_INFORMATION) -> Self {
        Self {
            process: OwnedHandle::new(info.hProcess).expect("CreateProcessW returned null process"),
            thread: Some(
                OwnedHandle::new(info.hThread).expect("CreateProcessW returned null thread"),
            ),
            exited: false,
        }
    }

    fn process(&self) -> HANDLE {
        self.process.raw()
    }

    fn thread(&self) -> HANDLE {
        self.thread.as_ref().expect("primary thread is live").raw()
    }

    fn close_thread(&mut self) {
        self.thread = None;
    }

    fn exit_code(&self) -> Result<u32, SandboxError> {
        let mut code = 0_u32;
        // SAFETY: the process handle is live and `code` is a valid out pointer.
        if unsafe { GetExitCodeProcess(self.process(), &mut code) } == 0 {
            Err(last_error("read sandbox worker exit code"))
        } else {
            Ok(code)
        }
    }

    fn mark_exited(&mut self) {
        self.exited = true;
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.exited {
            // SAFETY: the process handle is live; this prevents a suspended or
            // partially configured candidate from escaping an error path.
            unsafe { TerminateProcess(self.process(), TERMINATED_BY_SUPERVISOR) };
        }
    }
}

struct OwnedHandle(Option<HANDLE>);

impl OwnedHandle {
    fn new(handle: HANDLE) -> Option<Self> {
        (!handle.is_null()).then_some(Self(Some(handle)))
    }

    fn raw(&self) -> HANDLE {
        self.0.expect("handle is live")
    }

    fn into_file(mut self) -> File {
        let handle = self.0.take().expect("handle is live");
        // SAFETY: ownership of this valid Windows handle moves into `File`.
        unsafe { File::from_raw_handle(handle as RawHandle) }
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // SAFETY: this object uniquely owns the live handle.
            unsafe { CloseHandle(handle) };
        }
    }
}

fn pipe() -> Result<(OwnedHandle, OwnedHandle), SandboxError> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("SECURITY_ATTRIBUTES size fits in u32"),
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let mut read = null_mut();
    let mut write = null_mut();
    // SAFETY: handles are valid out pointers and attributes remain live.
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(last_error("create sandbox output pipe"));
    }
    let read = OwnedHandle::new(read).expect("CreatePipe returned null read handle");
    let write = OwnedHandle::new(write).expect("CreatePipe returned null write handle");
    // Only the write end belongs in the child's explicit handle list.
    // SAFETY: the read handle is live.
    if unsafe { SetHandleInformation(read.raw(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(last_error("make sandbox pipe reader non-inheritable"));
    }
    Ok((read, write))
}

fn inheritable_null_input() -> Result<OwnedHandle, SandboxError> {
    let file = OpenOptions::new()
        .read(true)
        .open("NUL")
        .map_err(|error| platform_error("open sandbox null input", error))?;
    let raw = file.as_raw_handle() as HANDLE;
    // SAFETY: `raw` remains live while the File is held.
    if unsafe { SetHandleInformation(raw, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
        return Err(last_error("make sandbox input inheritable"));
    }
    std::mem::forget(file);
    OwnedHandle::new(raw).ok_or_else(|| SandboxError::Platform("NUL returned null handle".into()))
}

fn spawn_reader(
    mut file: File,
    total: Arc<AtomicU64>,
    exceeded: Arc<AtomicBool>,
    limit: u64,
) -> JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut captured = Vec::new();
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            let read = file.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            let read_u64 = u64::try_from(read).expect("pipe chunk length fits in u64");
            let previous = total.fetch_add(read_u64, Ordering::AcqRel);
            let available = limit.saturating_sub(previous);
            let keep = usize::try_from(available.min(read_u64)).expect("bounded by chunk size");
            captured.extend_from_slice(&chunk[..keep]);
            if previous.saturating_add(read_u64) > limit {
                exceeded.store(true, Ordering::Release);
            }
        }
        Ok(captured)
    })
}

fn join_reader(
    reader: JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, SandboxError> {
    reader
        .join()
        .map_err(|_| SandboxError::Platform(format!("sandbox {stream} reader panicked")))?
        .map_err(|error| platform_error(&format!("read sandbox {stream}"), error))
}

fn build_command_line(executable: &Path, arguments: &[String]) -> Vec<u16> {
    let mut command = quote_windows_argument(&executable.as_os_str().to_string_lossy());
    for argument in arguments {
        command.push(' ');
        command.push_str(&quote_windows_argument(argument));
    }
    wide_null(OsStr::new(&command))
}

fn minimal_environment(profile_folder: &Path) -> Vec<u16> {
    let folder = profile_folder.as_os_str().to_string_lossy();
    let system_root = std::env::var_os("SystemRoot")
        .unwrap_or_else(|| OsString::from(r"C:\Windows"))
        .to_string_lossy()
        .into_owned();
    let mut entries = vec![
        "IRIS_NATIVE_SANDBOX=1".to_owned(),
        format!("LOCALAPPDATA={folder}"),
        format!("SystemRoot={system_root}"),
        format!("TEMP={folder}"),
        format!("TMP={folder}"),
        format!("WINDIR={system_root}"),
    ];
    entries.sort_by_key(|entry| entry.to_ascii_uppercase());

    let mut block = Vec::new();
    for entry in entries {
        block.extend(OsStr::new(&entry).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

fn quote_windows_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    let mut backslashes = 0_usize;
    for character in argument.chars() {
        if character == '\\' {
            backslashes += 1;
        } else if character == '"' {
            quoted.extend(std::iter::repeat('\\').take(backslashes * 2 + 1));
            quoted.push('"');
            backslashes = 0;
        } else {
            quoted.extend(std::iter::repeat('\\').take(backslashes));
            backslashes = 0;
            quoted.push(character);
        }
    }
    quoted.extend(std::iter::repeat('\\').take(backslashes * 2));
    quoted.push('"');
    quoted
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn wide_ptr_to_os_string(value: *const u16) -> OsString {
    let mut length = 0_usize;
    // SAFETY: callers provide a valid NUL-terminated Windows string.
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` was measured within the NUL-terminated allocation.
    OsString::from_wide(unsafe { std::slice::from_raw_parts(value, length) })
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn platform_error(action: &str, error: std::io::Error) -> SandboxError {
    SandboxError::Platform(format!("{action}: {error}"))
}

fn last_error(action: &str) -> SandboxError {
    // SAFETY: GetLastError reads the calling thread's last-error slot.
    let code = unsafe { GetLastError() };
    SandboxError::Platform(format!(
        "{action}: {} (Windows error {code})",
        std::io::Error::from_raw_os_error(code as i32)
    ))
}

use super::admission::Admission;
#[cfg(feature = "fault-injection")]
use super::admission::{is_create_post, is_delete_request, is_put_request};
use super::{Request, Response, SadtTransport, TransportError};
use crate::config::Connection;
use libloading::{Library, Symbol};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::mem::ManuallyDrop;
use std::os::raw::{c_int, c_uint};
use std::path::{Path, PathBuf};

pub const SADT_REST_RFC_ENDPOINT: &str = "SADT_REST_RFC_ENDPOINT";

/// The library reads this (through the C runtime's `getenv`) when it opens its log, which is its
/// first call.
pub const RFC_TRACE_DIR_ENV: &str = "RFC_TRACE_DIR";

const RFC_LIBRARY_OVERRIDE: &str = "ABAPREACH_RFC_LIBRARY";
pub const MAX_GUI_SEARCH_DEPTH: usize = 4;
/// How much of a candidate is read to decide its architecture; no library is mapped for it.
const HEADER_BYTES: usize = 4096;
/// The first four bytes of an ELF image.
pub const ELF_MAGIC: [u8; 4] = *b"\x7fELF";
/// The ELF header bytes the architecture check reads: `e_ident`, `e_type`, and `e_machine`.
const ELF_HEADER_BYTES: usize = 20;
const PE_SIGNATURE: [u8; 4] = *b"PE\0\0";
/// The PE header bytes that hold `e_lfanew`, which points at the COFF machine word.
pub const PE_OFFSET_END: usize = 0x40;
/// The stored magic of a universal Mach-O image.
const MACHO_UNIVERSAL: [u8; 4] = [0xca, 0xfe, 0xba, 0xbe];
/// `MH_MAGIC_64` and `MH_MAGIC` as a big-endian image stores them.
const MACHO_64_BE: [u8; 4] = [0xfe, 0xed, 0xfa, 0xcf];
const MACHO_32_BE: [u8; 4] = [0xfe, 0xed, 0xfa, 0xce];
/// `MH_CIGAM_64` and `MH_CIGAM` as a little-endian image stores them.
const MACHO_64_LE: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];
const MACHO_32_LE: [u8; 4] = [0xce, 0xfa, 0xed, 0xfe];
const MACHO_FAT_ARCH_BYTES: usize = 20;
const MAX_MACHO_SLICES: usize = 32;
/// The whole ELF64 header: `e_ident`, `e_type`, `e_machine`, and the program header fields.
const ELF64_HEADER_BYTES: usize = 64;
/// One ELF64 program header: `p_type`, `p_flags`, then six eight-byte fields.
const ELF64_PROGRAM_HEADER_BYTES: usize = 56;
const ELF64_DYNAMIC_ENTRY_BYTES: usize = 16;
/// The fields of one ELF64 program header this loader reads, at their offsets inside the entry.
const PHDR_TYPE: usize = 0;
const PHDR_OFFSET: usize = 8;
const PHDR_VADDR: usize = 16;
const PHDR_FILESZ: usize = 32;
const ELF64_LOAD: u32 = 1;
const ELF64_DYNAMIC: u32 = 2;
/// `DT_NULL`, `DT_STRTAB`, `DT_STRSZ`, `DT_RPATH`, and `DT_RUNPATH` in an ELF64 image.
const DT_NULL: i64 = 0;
const DT_STRTAB: i64 = 5;
const DT_STRSZ: i64 = 10;
const DT_RPATH: i64 = 15;
const DT_RUNPATH: i64 = 29;
/// The placeholder a dependency search path uses for the library's own directory.
const ELF_ORIGIN: &str = "$ORIGIN";
/// `LOAD_WITH_ALTERED_SEARCH_PATH`: `LoadLibraryExW` also searches the loaded DLL's own folder.
pub const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
const MACHO_X86: u32 = 0x0000_0007;
const MACHO_X86_64: u32 = 0x0100_0007;
const MACHO_ARM: u32 = 0x0000_000c;
const MACHO_ARM64: u32 = 0x0100_000c;
const MACOS_APPLICATIONS: &str = "/Applications";
const UNIX_SDK_ROOT: &str = "/opt/sap/nwrfcsdk";
const UNIX_LOCAL_SDK_ROOT: &str = "/usr/local/sap/nwrfcsdk";
const LINUX_GUI_ROOT: &str = "/opt/sapgui";
const LINUX_LOCAL_GUI_ROOT: &str = "/usr/local/sapgui";
#[cfg(feature = "fault-injection")]
const ACTIVATION_URI: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=true";
const RFC_OK: c_int = 0;
const RFC_COMMUNICATION_FAILURE: c_int = 1;
const RFC_LOGON_FAILURE: c_int = 2;
const RFC_ABAP_RUNTIME_FAILURE: c_int = 3;
const RFC_ABAP_MESSAGE: c_int = 4;
const RFC_ABAP_EXCEPTION: c_int = 5;
const RFC_ABAP_CLASS_EXCEPTION: c_int = 27;
const RFC_AUTHORIZATION_FAILURE: c_int = 29;
/// `RFC_INVALID_PARAMETER`: the code reported for a text the library's character set cannot hold.
const RFC_NOT_REPRESENTABLE: c_int = 20;

type SapUc = u16;
type RfcHandle = *mut c_void;
type RfcRc = c_int;

#[derive(Clone, Debug, Default)]
pub struct Env {
    override_path: Option<PathBuf>,
    gui_paths: Vec<PathBuf>,
    sdk_paths: Vec<PathBuf>,
    /// Discovery decisions the doctor must show, such as a location that was skipped.
    notes: Vec<String>,
}

impl Env {
    /// Discovery for the platform this binary was built for, from the process environment.
    pub fn current() -> Self {
        let env = std::env::vars_os()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();

        Self::discover(Platform::current(), &env, &SystemFs)
    }

    /// Resolves the explicit override and the automatic candidates for one platform; the
    /// platform and the file-system queries are parameters so every one of them is testable.
    pub fn discover(
        platform: Platform,
        env: &BTreeMap<String, String>,
        fs: &dyn CandidateFs,
    ) -> Self {
        let gui = gui_candidates(platform, env, fs);

        Self {
            override_path: non_empty(env, RFC_LIBRARY_OVERRIDE).map(PathBuf::from),
            gui_paths: gui.candidates,
            sdk_paths: sdk_candidates(platform, env),
            notes: gui.notes,
        }
    }

    /// The discovery decisions a user must see; empty when nothing was skipped.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    #[cfg(feature = "test-support")]
    pub fn test_paths(override_path: &str, gui_path: &str, sdk_path: &str) -> Self {
        Self {
            override_path: Some(PathBuf::from(override_path)),
            gui_paths: vec![PathBuf::from(gui_path)],
            sdk_paths: vec![PathBuf::from(sdk_path)],
            notes: Vec::new(),
        }
    }
}

pub struct RfcLibrary {
    path: PathBuf,
    arch: Arch,
    family: Family,
    source: Origin,
    /// What `RfcSetTraceDir` did when the library was loaded.
    trace: TraceTarget,
    /// How many bytes one `SAP_UC` of this library takes; decided before any string is passed.
    width: UcWidth,
    /// Runtime loader warnings that doctor must show without making the candidate fail.
    notes: Vec<String>,
    // Never unloaded: SAP's library leaves thread-local destructors that crash after dlclose, and
    // the dependencies preloaded for it have to stay mapped just as long.
    loaded: Loaded,
}

impl RfcLibrary {
    pub fn candidate_paths(env: &Env) -> Vec<PathBuf> {
        Self::candidates(env)
            .into_iter()
            .map(|candidate| candidate.path)
            .collect()
    }

    /// Every candidate in load order, each labelled with its family and the origin that decides
    /// whether it may fall back.
    pub fn candidates(env: &Env) -> Vec<Candidate> {
        let discovered = |family: Family| {
            move |path: &PathBuf| Candidate::located(path.clone(), family, Origin::Discovered)
        };

        env.override_path
            .iter()
            .map(|path| Candidate::new(path.clone(), Origin::Explicit))
            .chain(env.gui_paths.iter().map(discovered(Family::Gui)))
            .chain(env.sdk_paths.iter().map(discovered(Family::Sdk)))
            .collect()
    }

    /// Loads the first candidate whose header matches this binary; the decision never tries a
    /// candidate after a failure on a path the user named.
    pub fn load(env: &Env) -> Result<Self, String> {
        let candidates = Self::candidates(env);

        if candidates.is_empty() {
            return Err("RFC library not found; set ABAPREACH_RFC_LIBRARY".to_owned());
        }

        let (chosen, loaded) =
            decide(&candidates, load_candidate).map_err(|error| error.to_string())?;

        // The width comes first: `RfcSetTraceDir` takes a string in the library's own character
        // size. `RfcGetVersion` takes no string and, with `RFC_TRACE_DIR` already set by `main`,
        // its log file already lands in the temp dir.
        let width = detect_width(loaded.library());
        let trace = set_trace_directory(loaded.library(), width);

        Ok(Self {
            path: loaded.path().to_path_buf(),
            arch: loaded.arch(),
            family: candidates[chosen].family,
            source: candidates[chosen].origin,
            trace,
            width,
            notes: loaded.notes().to_vec(),
            loaded,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Non-blocking runtime loader warnings, for example a missing optional ICU set.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The SDK version reported by `RfcGetVersion`; no connection is opened for it.
    pub fn version(&self) -> Result<(u32, u32, u32), String> {
        let get_version: RfcGetVersion = load_symbol(self.library(), b"RfcGetVersion\0")?;
        let (mut major, mut minor, mut patch) = (0, 0, 0);

        unsafe {
            get_version(&mut major, &mut minor, &mut patch);
        }

        Ok((major, minor, patch))
    }

    /// What the doctor reports: the path, the version the library reports, the architecture the
    /// header declares, the family the file name names, and where the path came from.
    pub fn identity(&self) -> Result<LibraryIdentity, String> {
        let (major, minor, patch) = self.version()?;

        Ok(LibraryIdentity::new(
            self.path.clone(),
            format!("{major}.{minor}.{patch}"),
            self.arch,
            self.family,
            self.source,
            self.width,
        ))
    }

    pub fn library(&self) -> &Library {
        self.loaded.library()
    }

    /// Whether `RfcSetTraceDir` is exported and what it returned for this library.
    pub fn trace_target(&self) -> TraceTarget {
        self.trace
    }

    /// The size of one `SAP_UC` in this library.
    pub fn width(&self) -> UcWidth {
        self.width
    }
}

/// The SAP library writes `dev_rfc.log` into the cwd; its trace belongs in the temp dir.
pub fn trace_directory() -> PathBuf {
    std::env::temp_dir()
}

/// The variable and the value that point the library at the temp dir before it is mapped.
pub fn trace_environment() -> (&'static str, String) {
    (RFC_TRACE_DIR_ENV, trace_directory().display().to_string())
}

/// Sets that variable. `set_var` is only sound while no other thread can read the environment,
/// so `main` calls this first, before `ctrlc` starts its handler thread; `RfcLibrary::load` never
/// touches the environment itself.
pub fn init_trace_environment() {
    let (name, directory) = trace_environment();

    // Not thread safe by contract; the caller runs before any thread exists.
    unsafe { std::env::set_var(name, &directory) };
    #[cfg(windows)]
    set_crt_environment(name, &directory);
}

/// `LOAD_LIBRARY_SEARCH_SYSTEM32`: a bare system DLL name is looked up in System32 only, never in
/// the working directory.
#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

/// Rust's `set_var` calls only `SetEnvironmentVariableW`, but the SAP libraries are C and read the
/// environment through `getenv` of the shared UCRT (`ucrtbase.dll`, which both SAP DLLs import).
/// That copy is not the Win32 block: a dynamic-CRT executable fills it once at start-up, so a later
/// `set_var` never reaches it, and a static-CRT executable (the release build) never fills it at
/// all, so the library saw no `RFC_TRACE_DIR` and opened `dev_rfc.log` in the working directory on
/// its first call. The write goes through the `_wputenv_s` of that very `ucrtbase.dll` instead of
/// this executable's own CRT, so it reaches the library's copy in both builds. The caller has the
/// same single-thread contract as `set_var`.
#[cfg(windows)]
fn set_crt_environment(name: &str, value: &str) {
    type PutEnv = unsafe extern "C" fn(*const u16, *const u16) -> c_int;

    let Ok(ucrt) = (unsafe {
        libloading::os::windows::Library::load_with_flags(
            "ucrtbase.dll",
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    }) else {
        return;
    };
    let (name, value) = (sap_uc(name), sap_uc(value));

    // A failure leaves the environment as `set_var` set it, and `RfcSetTraceDir` still follows.
    if let Ok(put_env) = unsafe { ucrt.get::<PutEnv>(b"_wputenv_s\0") } {
        let _ = unsafe { put_env(name.as_ptr(), value.as_ptr()) };
    }

    // Never unloaded: the copy of the environment lives in this module instance, and the SAP
    // library must find that instance, with the variable in it, when it is mapped later.
    std::mem::forget(ucrt);
}

/// Runs `install` (the signal handler registration) only after the trace environment is set, so
/// the order cannot be swapped by accident.
pub fn after_trace_environment<T>(install: impl FnOnce() -> T) -> T {
    init_trace_environment();

    install()
}

/// `RfcSetTraceDir` is optional: not every SDK or GUI release exports it.
fn set_trace_directory(library: &Library, width: UcWidth) -> TraceTarget {
    let set_trace_dir = unsafe { library.get::<RfcSetTraceDir>(b"RfcSetTraceDir\0") }
        .ok()
        .map(|symbol| *symbol);

    apply_trace_directory(set_trace_dir, width)
}

/// Calls `RfcSetTraceDir` when the library exports it, otherwise the environment set at the
/// start of `main` stays in charge, so `dev_rfc.log` never lands in the working directory. A
/// returned error is reported, not hidden: the environment is then the only protection left.
pub fn apply_trace_directory(set_trace_dir: Option<RfcSetTraceDir>, width: UcWidth) -> TraceTarget {
    apply_trace_directory_to(
        set_trace_dir,
        width,
        &trace_directory().display().to_string(),
    )
}

/// `apply_trace_directory` for a named directory, so the width decisions are testable with any
/// path.
pub fn apply_trace_directory_to(
    set_trace_dir: Option<RfcSetTraceDir>,
    width: UcWidth,
    directory: &str,
) -> TraceTarget {
    let Some(set_trace_dir) = set_trace_dir else {
        return TraceTarget::Environment;
    };

    // A directory the library cannot read is never passed: a wrong string would move the trace
    // somewhere else, so the environment stays in charge.
    let Ok(directory) = UcString::encode(width, directory) else {
        return TraceTarget::NotEncodable;
    };
    let mut error = RfcErrorInfo::default();

    let result = unsafe { set_trace_dir(directory.as_ptr(), (&raw mut error).cast::<c_void>()) };

    if result == RFC_OK {
        TraceTarget::Symbol
    } else {
        TraceTarget::Rejected(result)
    }
}

pub fn sap_uc(value: &str) -> Vec<u16> {
    let mut units = value.encode_utf16().collect::<Vec<_>>();

    units.push(0);
    units
}

pub fn from_sap_uc(units: &[u16]) -> Result<String, String> {
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());

    String::from_utf16(&units[..end]).map_err(|error| error.to_string())
}

/// The size of one `SAP_UC`. NW RFC SDK libraries and the macOS and Linux SAP GUI libraries use
/// UTF-16, but the SAP GUI 8.00 for Windows `sapnwrfccm.dll` is a single-byte build: it reads every
/// string argument as `char*`, so a UTF-16 name such as `ASHOST` reaches it as `A`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UcWidth {
    /// Two bytes per character, UTF-16.
    Utf16,
    /// One byte per character.
    Narrow,
}

impl UcWidth {
    /// The width a library uses, from the first bytes of the string `RfcGetVersion` returns. That
    /// text starts with at least two printable characters, so a zero second byte means UTF-16.
    /// Anything that cannot be told apart reads as UTF-16, the width of every other library.
    pub fn detect(version_text: &[u8]) -> Self {
        match version_text {
            [first, second, ..] if *first != 0 && *second != 0 => Self::Narrow,
            _ => Self::Utf16,
        }
    }

    /// How a message and the doctor name the width.
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf16 => "UTF-16",
            Self::Narrow => "single-byte",
        }
    }
}

/// Reads the width from the version text the library itself returns; `RfcGetVersion` takes no
/// string, so it is safe to call before the width is known.
fn detect_width(library: &Library) -> UcWidth {
    let Ok(get_version) = load_symbol::<RfcGetVersion>(library, b"RfcGetVersion\0") else {
        return UcWidth::Utf16;
    };
    let (mut major, mut minor, mut patch) = (0, 0, 0);
    let text = unsafe { get_version(&mut major, &mut minor, &mut patch) }.cast::<u8>();

    if text.is_null() {
        return UcWidth::Utf16;
    }

    // Two bytes are always readable: a first byte of zero ends the string, and the terminator
    // itself is the second byte.
    let first = unsafe { *text };
    let second = if first == 0 {
        0
    } else {
        unsafe { *text.add(1) }
    };

    UcWidth::detect(&[first, second])
}

/// A NUL-terminated string in the character size of the library that receives it.
pub enum UcString {
    Utf16(Vec<u16>),
    Narrow(Vec<u8>),
}

impl UcString {
    /// A single-byte library accepts only ASCII here: its code page is not known, and a wrong
    /// byte in a name, a URI or a password must not reach SAP.
    pub fn encode(width: UcWidth, value: &str) -> Result<Self, String> {
        match width {
            UcWidth::Utf16 => Ok(Self::Utf16(sap_uc(value))),
            UcWidth::Narrow if value.is_ascii() => {
                let mut bytes = value.as_bytes().to_vec();

                // Two terminators: a library that misjudges its width and reads UTF-16 (or a
                // wide string over this one) still finds the end inside the buffer.
                bytes.extend([0, 0]);
                Ok(Self::Narrow(bytes))
            }
            UcWidth::Narrow => Err(format!(
                "a {} RFC library accepts ASCII only; the text contains other characters; use the NW RFC SDK library (ABAPREACH_RFC_LIBRARY=...\\sapnwrfc.dll) instead",
                UcWidth::Narrow.label()
            )),
        }
    }

    pub fn as_ptr(&self) -> *const c_void {
        match self {
            Self::Utf16(units) => units.as_ptr().cast(),
            Self::Narrow(bytes) => bytes.as_ptr().cast(),
        }
    }

    /// The length in characters, without the terminator.
    pub fn chars(&self) -> usize {
        match self {
            Self::Utf16(units) => units.len() - 1,
            Self::Narrow(bytes) => bytes.len() - 2,
        }
    }
}

/// A buffer the library fills with characters.
enum UcBuffer {
    Utf16(Vec<u16>),
    Narrow(Vec<u8>),
}

impl UcBuffer {
    fn new(width: UcWidth, chars: usize) -> Self {
        match width {
            UcWidth::Utf16 => Self::Utf16(vec![0; chars]),
            UcWidth::Narrow => Self::Narrow(vec![0; chars]),
        }
    }

    fn chars(&self) -> usize {
        match self {
            Self::Utf16(units) => units.len(),
            Self::Narrow(bytes) => bytes.len(),
        }
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        match self {
            Self::Utf16(units) => units.as_mut_ptr().cast(),
            Self::Narrow(bytes) => bytes.as_mut_ptr().cast(),
        }
    }

    /// The first `written` characters as text.
    fn text(&self, written: usize) -> Result<String, String> {
        match self {
            Self::Utf16(units) => from_sap_uc(&units[..written.min(units.len())]),
            Self::Narrow(bytes) => Ok(narrow_text(&bytes[..written.min(bytes.len())])),
        }
    }
}

/// Single-byte text up to the first NUL; a byte outside ASCII shows as the replacement character.
fn narrow_text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());

    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RfcEndpointRequest {
    pub function: &'static str,
    pub method: String,
    pub uri: String,
    pub version: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RfcEndpointResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub trait RfcApi {
    fn open(&mut self) -> Result<(), TransportError>;

    fn invoke(
        &mut self,
        request: RfcEndpointRequest,
    ) -> Result<RfcEndpointResponse, TransportError>;

    fn close(&mut self);
}

pub struct RfcTransport<A> {
    api: A,
    opened: bool,
    /// The request boundary and the state of the activation protocol it keeps.
    admission: Admission,
    /// Test-only: the write whose reply is discarded after SAP executed it, and the read that fails.
    #[cfg(feature = "fault-injection")]
    faults: Vec<crate::fault::Fault>,
    /// Test-only: a create `POST` has been admitted, so the next `GET` is its reconciliation read.
    #[cfg(feature = "fault-injection")]
    create_sent: bool,
}

impl<A> RfcTransport<A> {
    pub fn new(api: A) -> Self {
        Self {
            api,
            opened: false,
            admission: Admission::default(),
            #[cfg(feature = "fault-injection")]
            faults: crate::fault::Fault::from_env(),
            #[cfg(feature = "fault-injection")]
            create_sent: false,
        }
    }

    #[cfg(feature = "fault-injection")]
    pub fn with_fault(api: A, fault: Option<crate::fault::Fault>) -> Self {
        Self::with_faults(api, fault.into_iter().collect())
    }

    #[cfg(feature = "fault-injection")]
    pub fn with_faults(api: A, faults: Vec<crate::fault::Fault>) -> Self {
        Self {
            api,
            opened: false,
            admission: Admission::default(),
            faults,
            create_sent: false,
        }
    }

    pub fn api(&self) -> &A {
        &self.api
    }
}

impl<A: RfcApi> RfcTransport<A> {
    fn ensure_open(&mut self) -> Result<(), TransportError> {
        if !self.opened {
            self.api.open()?;
            self.opened = true;
        }

        Ok(())
    }
}

impl<A: RfcApi> SadtTransport for RfcTransport<A> {
    fn request(&mut self, request: Request) -> Result<Response, TransportError> {
        if !self.admission.admits(&request) {
            self.admission.clear();

            return Err(TransportError::RequestNotAllowed(format!(
                "{} {}",
                request.method, request.uri
            )));
        }

        if let Err(error) = self.ensure_open() {
            self.admission.clear();

            return Err(error);
        }
        // Decided before the request is consumed by the call.
        #[cfg(feature = "fault-injection")]
        let read_is_reconciliation =
            request.method == "GET" && (self.create_sent || crate::fault::delete_reply_dropped());
        #[cfg(feature = "fault-injection")]
        if is_create_post(&request) {
            self.create_sent = true;
        }
        #[cfg(feature = "fault-injection")]
        if let Some(error) = self
            .faults
            .iter()
            .find_map(|fault| fault.fail_read(read_is_reconciliation))
        {
            self.admission.clear();

            return Err(error);
        }
        #[cfg(feature = "fault-injection")]
        let dropped = self.faults.iter().find_map(|fault| {
            fault
                .target()
                .and_then(|target| fault.drop_reply(is_fault_target(target, &request)))
        });
        #[cfg(feature = "fault-injection")]
        let delete_dropped = dropped.is_some() && is_delete_request(&request);
        let response = match self.api.invoke(RfcEndpointRequest {
            function: SADT_REST_RFC_ENDPOINT,
            method: request.method,
            uri: request.uri,
            version: "HTTP/1.1".to_owned(),
            headers: request.headers,
            body: request.body,
        }) {
            Ok(response) => response,
            Err(error) => {
                self.admission.clear();

                return Err(error);
            }
        };

        self.admission
            .received_response(response.status, &response.body);

        // The RFC call has run and SAP has executed the write; only the reply is discarded.
        #[cfg(feature = "fault-injection")]
        if let Some(error) = dropped {
            self.admission.clear();

            if delete_dropped {
                crate::fault::note_delete_reply_dropped();
            }

            return Err(error);
        }

        Ok(Response {
            status: response.status,
            headers: response.headers,
            body: response.body,
        })
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.admission.clear();

        if self.opened {
            self.api.close();
            self.opened = false;
        }

        Ok(())
    }
}

#[cfg(feature = "test-support")]
pub struct FakeRfcApi {
    opens: usize,
    closes: usize,
    invocations: usize,
    last_request: Option<RfcEndpointRequest>,
    responses: std::collections::VecDeque<RfcEndpointResponse>,
}

#[cfg(feature = "test-support")]
impl FakeRfcApi {
    pub fn ok() -> Self {
        Self {
            opens: 0,
            closes: 0,
            invocations: 0,
            last_request: None,
            responses: std::collections::VecDeque::new(),
        }
    }

    pub fn with_responses(responses: Vec<RfcEndpointResponse>) -> Self {
        Self {
            responses: responses.into(),
            ..Self::ok()
        }
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        (self.opens, self.closes, self.invocations)
    }

    pub fn last_request(&self) -> &RfcEndpointRequest {
        self.last_request
            .as_ref()
            .expect("RFC endpoint was not invoked")
    }
}

#[cfg(feature = "test-support")]
impl RfcApi for FakeRfcApi {
    fn open(&mut self) -> Result<(), TransportError> {
        self.opens += 1;

        Ok(())
    }

    fn invoke(
        &mut self,
        request: RfcEndpointRequest,
    ) -> Result<RfcEndpointResponse, TransportError> {
        self.invocations += 1;
        self.last_request = Some(request);

        Ok(self.responses.pop_front().unwrap_or(RfcEndpointResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Vec::new(),
        }))
    }

    fn close(&mut self) {
        self.closes += 1;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RfcPhase {
    /// Opening the connection and ping: nothing of ours has been sent.
    Open,
    /// Function metadata and request marshalling before `RfcInvoke`.
    Prepare,
    /// `RfcInvoke` and reading its response: the request may have reached SAP.
    Invoke,
}

/// Maps an `RFC_RC` code to a transport error; only `PreSendConnectivity` allows a fallback.
pub fn classify_rfc_error(phase: RfcPhase, code: i32, message: &str) -> TransportError {
    let text = format!("{message} (RFC code {code})");

    let error = match code {
        RFC_LOGON_FAILURE | RFC_AUTHORIZATION_FAILURE => TransportError::Authorization(text),
        RFC_ABAP_RUNTIME_FAILURE
        | RFC_ABAP_MESSAGE
        | RFC_ABAP_EXCEPTION
        | RFC_ABAP_CLASS_EXCEPTION => {
            if message.to_ascii_lowercase().contains("authori") {
                TransportError::Authorization(text)
            } else {
                TransportError::Protocol(text)
            }
        }
        _ => match phase {
            RfcPhase::Open => TransportError::PreSendConnectivity(text),
            RfcPhase::Prepare if code == RFC_COMMUNICATION_FAILURE => {
                TransportError::PreSendConnectivity(text)
            }
            RfcPhase::Prepare => TransportError::Protocol(text),
            RfcPhase::Invoke => TransportError::PostSendUncertain(text),
        },
    };

    if phase == RfcPhase::Invoke {
        error.after_send()
    } else {
        error
    }
}

/// Logon parameters in SDK naming; the password is only ever passed to `RfcOpenConnection`.
/// A connection without an `rfc` block has no parameters and can never be opened.
pub fn connection_parameters(connection: &Connection) -> Result<Vec<(String, String)>, String> {
    let Some(rfc) = &connection.rfc else {
        return Err(format!(
            "RFC not configured for connection {}",
            connection.id
        ));
    };
    let mut parameters = vec![
        ("ASHOST".to_owned(), rfc.ashost.clone()),
        ("SYSNR".to_owned(), rfc.sysnr.clone()),
        ("CLIENT".to_owned(), connection.client.clone()),
        ("USER".to_owned(), connection.user.clone()),
        ("PASSWD".to_owned(), connection.password.clone()),
        ("LANG".to_owned(), connection.lang.clone()),
    ];

    if let Some(router) = &rfc.saprouter {
        parameters.push(("SAPROUTER".to_owned(), router.clone()));
    }

    Ok(parameters)
}

/// Both pointers address strings in the library's character size.
#[repr(C)]
struct RfcConnectionParameter {
    name: *const c_void,
    value: *const c_void,
}

#[repr(C)]
struct RfcErrorInfo {
    code: c_int,
    group: c_int,
    key: [SapUc; 128],
    message: [SapUc; 512],
    abap_msg_class: [SapUc; 21],
    abap_msg_type: [SapUc; 2],
    abap_msg_number: [SapUc; 4],
    abap_msg_v1: [SapUc; 51],
    abap_msg_v2: [SapUc; 51],
    abap_msg_v3: [SapUc; 51],
    abap_msg_v4: [SapUc; 51],
}

/// Where a single-byte library stores the error text: after `code`, `group`, and a 128-byte key.
const NARROW_MESSAGE_OFFSET: usize = 8 + 128;
const NARROW_MESSAGE_BYTES: usize = 512;

/// Where a single-byte library stores the error key: right after `code` and `group`.
const NARROW_KEY_OFFSET: usize = 8;
const NARROW_KEY_BYTES: usize = 128;

impl RfcErrorInfo {
    /// The error key, for example the ABAP exception name; empty when the library set none.
    fn key(&self, width: UcWidth) -> String {
        match width {
            UcWidth::Utf16 => from_sap_uc(&self.key).unwrap_or_default(),
            UcWidth::Narrow => {
                // `key` is a plain `u16` array, so every byte read here is initialized, and the
                // range ends inside `key`.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        std::ptr::from_ref(self).cast::<u8>().add(NARROW_KEY_OFFSET),
                        NARROW_KEY_BYTES,
                    )
                };

                narrow_text(bytes)
            }
        }
    }

    /// The error text. A single-byte library fills the same memory with a denser layout, so its
    /// text is read from the byte offset the layout gives, not from the UTF-16 field.
    fn text(&self, width: UcWidth) -> String {
        match width {
            UcWidth::Utf16 => {
                from_sap_uc(&self.message).unwrap_or_else(|_| "unreadable error text".to_owned())
            }
            UcWidth::Narrow => {
                // `key` and `message` are plain `u16` arrays, so every byte read here is
                // initialized, and the range ends inside `message`.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        std::ptr::from_ref(self)
                            .cast::<u8>()
                            .add(NARROW_MESSAGE_OFFSET),
                        NARROW_MESSAGE_BYTES,
                    )
                };

                narrow_text(bytes)
            }
        }
    }
}

impl Default for RfcErrorInfo {
    fn default() -> Self {
        Self {
            code: 0,
            group: 0,
            key: [0; 128],
            message: [0; 512],
            abap_msg_class: [0; 21],
            abap_msg_type: [0; 2],
            abap_msg_number: [0; 4],
            abap_msg_v1: [0; 51],
            abap_msg_v2: [0; 51],
            abap_msg_v3: [0; 51],
            abap_msg_v4: [0; 51],
        }
    }
}

type RfcOpenConnection =
    unsafe extern "C" fn(*const RfcConnectionParameter, c_uint, *mut RfcErrorInfo) -> RfcHandle;
type RfcPing = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcGetVersion = unsafe extern "C" fn(*mut c_uint, *mut c_uint, *mut c_uint) -> *const c_void;
type RfcGetFunctionDesc =
    unsafe extern "C" fn(RfcHandle, *const c_void, *mut RfcErrorInfo) -> RfcHandle;
type RfcCreateFunction = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcHandle;
type RfcGetStructure =
    unsafe extern "C" fn(RfcHandle, *const c_void, *mut RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcGetTable =
    unsafe extern "C" fn(RfcHandle, *const c_void, *mut RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcAppendNewRow = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcHandle;
type RfcSetString = unsafe extern "C" fn(
    RfcHandle,
    *const c_void,
    *const c_void,
    c_uint,
    *mut RfcErrorInfo,
) -> RfcRc;
type RfcSetXString = unsafe extern "C" fn(
    RfcHandle,
    *const c_void,
    *const c_void,
    c_uint,
    *mut RfcErrorInfo,
) -> RfcRc;
type RfcGetStringLength =
    unsafe extern "C" fn(RfcHandle, *const c_void, *mut c_uint, *mut RfcErrorInfo) -> RfcRc;
type RfcGetString = unsafe extern "C" fn(
    RfcHandle,
    *const c_void,
    *mut c_void,
    c_uint,
    *mut c_uint,
    *mut RfcErrorInfo,
) -> RfcRc;
type RfcGetXString = unsafe extern "C" fn(
    RfcHandle,
    *const c_void,
    *mut c_void,
    c_uint,
    *mut c_uint,
    *mut RfcErrorInfo,
) -> RfcRc;
type RfcInvoke = unsafe extern "C" fn(RfcHandle, RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcDestroyFunction = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcCloseConnection = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcSetTraceDir = unsafe extern "C" fn(*const c_void, *mut c_void) -> RfcRc;
type RfcGetRowCount = unsafe extern "C" fn(RfcHandle, *mut c_uint, *mut RfcErrorInfo) -> RfcRc;
type RfcMoveTo = unsafe extern "C" fn(RfcHandle, c_uint, *mut RfcErrorInfo) -> RfcRc;
type RfcGetCurrentRow = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcHandle;
type RfcGetInt =
    unsafe extern "C" fn(RfcHandle, *const c_void, *mut c_int, *mut RfcErrorInfo) -> RfcRc;
type RfcGetChars =
    unsafe extern "C" fn(RfcHandle, *const c_void, *mut c_void, c_uint, *mut RfcErrorInfo) -> RfcRc;
type RfcSetChars = unsafe extern "C" fn(
    RfcHandle,
    *const c_void,
    *const c_void,
    c_uint,
    *mut RfcErrorInfo,
) -> RfcRc;
type RfcGetParameterCount =
    unsafe extern "C" fn(RfcHandle, *mut c_uint, *mut RfcErrorInfo) -> RfcRc;
type RfcGetParameterDescByIndex =
    unsafe extern "C" fn(RfcHandle, c_uint, *mut c_void, *mut RfcErrorInfo) -> RfcRc;

/// Where the trace directory was set; the environment is the fallback when the symbol is absent
/// or returned an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceTarget {
    /// `RfcSetTraceDir` is exported and succeeded.
    Symbol,
    /// `RfcSetTraceDir` is not exported.
    Environment,
    /// `RfcSetTraceDir` is exported and returned this `RFC_RC`.
    Rejected(c_int),
    /// `RfcSetTraceDir` is exported, but the directory cannot be written in the library's
    /// character size, so it was not called.
    NotEncodable,
}

impl TraceTarget {
    /// The stable doctor line that records the outcome per library; the live matrix reads it.
    pub fn label(self) -> String {
        match self {
            Self::Symbol => "RFC trace directory: RfcSetTraceDir exported, set".to_owned(),
            Self::Environment => {
                "RFC trace directory: RfcSetTraceDir not exported, environment only".to_owned()
            }
            Self::Rejected(result) => format!(
                "RFC trace directory: RfcSetTraceDir exported, returned rc {result}, environment only"
            ),
            Self::NotEncodable => "RFC trace directory: RfcSetTraceDir exported, not called (the temp directory is not ASCII and the library uses single-byte characters), environment only".to_owned(),
        }
    }
}

pub struct RfcSymbols {
    open_connection: RfcOpenConnection,
    ping: RfcPing,
    get_version: RfcGetVersion,
    get_function_desc: RfcGetFunctionDesc,
    create_function: RfcCreateFunction,
    get_structure: RfcGetStructure,
    get_table: RfcGetTable,
    append_new_row: RfcAppendNewRow,
    set_string: RfcSetString,
    set_xstring: RfcSetXString,
    get_string_length: RfcGetStringLength,
    get_string: RfcGetString,
    get_xstring: RfcGetXString,
    invoke: RfcInvoke,
    destroy_function: RfcDestroyFunction,
    close_connection: RfcCloseConnection,
}

impl RfcSymbols {
    pub fn required_names() -> Vec<&'static str> {
        vec![
            "RfcOpenConnection",
            "RfcPing",
            "RfcGetVersion",
            "RfcGetFunctionDesc",
            "RfcCreateFunction",
            "RfcGetStructure",
            "RfcGetTable",
            "RfcAppendNewRow",
            "RfcSetString",
            "RfcSetXString",
            "RfcGetStringLength",
            "RfcGetString",
            "RfcGetXString",
            "RfcInvoke",
            "RfcDestroyFunction",
            "RfcCloseConnection",
        ]
    }

    /// The symbols the function-module path loads optionally.
    pub fn optional_names() -> Vec<&'static str> {
        FUNCTION_SYMBOL_NAMES.to_vec()
    }

    fn load(library: &Library) -> Result<Self, String> {
        Ok(Self {
            open_connection: load_symbol(library, b"RfcOpenConnection\0")?,
            ping: load_symbol(library, b"RfcPing\0")?,
            get_version: load_symbol(library, b"RfcGetVersion\0")?,
            get_function_desc: load_symbol(library, b"RfcGetFunctionDesc\0")?,
            create_function: load_symbol(library, b"RfcCreateFunction\0")?,
            get_structure: load_symbol(library, b"RfcGetStructure\0")?,
            get_table: load_symbol(library, b"RfcGetTable\0")?,
            append_new_row: load_symbol(library, b"RfcAppendNewRow\0")?,
            set_string: load_symbol(library, b"RfcSetString\0")?,
            set_xstring: load_symbol(library, b"RfcSetXString\0")?,
            get_string_length: load_symbol(library, b"RfcGetStringLength\0")?,
            get_string: load_symbol(library, b"RfcGetString\0")?,
            get_xstring: load_symbol(library, b"RfcGetXString\0")?,
            invoke: load_symbol(library, b"RfcInvoke\0")?,
            destroy_function: load_symbol(library, b"RfcDestroyFunction\0")?,
            close_connection: load_symbol(library, b"RfcCloseConnection\0")?,
        })
    }
}

fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
    let symbol: Symbol<T> = unsafe { library.get(name) }.map_err(|error| {
        let name = String::from_utf8_lossy(&name[..name.len() - 1]).into_owned();
        format!("RFC symbol {name}: {error}")
    })?;

    Ok(*symbol)
}

/// The symbols only the function-module path needs. They load optionally: a library that
/// lacks one still serves ADT and SADT, and only a function call reports the missing names.
#[derive(Clone, Copy)]
struct FunctionSymbols {
    get_row_count: RfcGetRowCount,
    move_to: RfcMoveTo,
    get_current_row: RfcGetCurrentRow,
    get_int: RfcGetInt,
    get_chars: RfcGetChars,
    set_chars: RfcSetChars,
    get_parameter_count: RfcGetParameterCount,
    get_parameter_desc_by_index: RfcGetParameterDescByIndex,
}

const FUNCTION_SYMBOL_NAMES: [&str; 8] = [
    "RfcGetRowCount",
    "RfcMoveTo",
    "RfcGetCurrentRow",
    "RfcGetInt",
    "RfcGetChars",
    "RfcSetChars",
    "RfcGetParameterCount",
    "RfcGetParameterDescByIndex",
];

fn optional_symbol<T: Copy>(
    library: &Library,
    name: &'static str,
    missing: &mut Vec<&'static str>,
) -> Option<T> {
    let symbol = load_symbol(library, format!("{name}\0").as_bytes()).ok();

    if symbol.is_none() {
        missing.push(name);
    }

    symbol
}

impl FunctionSymbols {
    /// All symbols, or the names of the missing ones.
    fn load(library: &Library) -> Result<Self, Vec<&'static str>> {
        let mut missing = Vec::new();
        let [a, b, c, d, e, f, g, h] = FUNCTION_SYMBOL_NAMES;
        let loaded = (
            optional_symbol(library, a, &mut missing),
            optional_symbol(library, b, &mut missing),
            optional_symbol(library, c, &mut missing),
            optional_symbol(library, d, &mut missing),
            optional_symbol(library, e, &mut missing),
            optional_symbol(library, f, &mut missing),
            optional_symbol(library, g, &mut missing),
            optional_symbol(library, h, &mut missing),
        );

        match loaded {
            (
                Some(get_row_count),
                Some(move_to),
                Some(get_current_row),
                Some(get_int),
                Some(get_chars),
                Some(set_chars),
                Some(get_parameter_count),
                Some(get_parameter_desc_by_index),
            ) => Ok(Self {
                get_row_count,
                move_to,
                get_current_row,
                get_int,
                get_chars,
                set_chars,
                get_parameter_count,
                get_parameter_desc_by_index,
            }),
            _ => Err(missing),
        }
    }
}

fn rfc_error(
    width: UcWidth,
    phase: RfcPhase,
    operation: &str,
    error: &RfcErrorInfo,
) -> TransportError {
    let message = error.text(width);

    classify_rfc_error(phase, error.code, &format!("{operation}: {message}"))
}

fn check(
    width: UcWidth,
    phase: RfcPhase,
    operation: &str,
    result: RfcRc,
    error: &RfcErrorInfo,
) -> Result<(), TransportError> {
    if result == RFC_OK {
        Ok(())
    } else {
        Err(rfc_error(width, phase, operation, error))
    }
}

/// A name or value in the library's character size, as the error of the phase that needs it.
fn encode(width: UcWidth, phase: RfcPhase, text: &str) -> Result<UcString, TransportError> {
    UcString::encode(width, text).map_err(|reason| {
        classify_rfc_error(phase, RFC_NOT_REPRESENTABLE, &format!("RFC text: {reason}"))
    })
}

pub struct DynamicRfcApi {
    // Keeps the library mapped for as long as the copied function pointers are used.
    _library: RfcLibrary,
    symbols: RfcSymbols,
    // The function-module symbols, or the names the library does not export.
    function: Result<FunctionSymbols, Vec<&'static str>>,
    width: UcWidth,
    // Owns the buffers that `parameters` points into.
    _parameter_storage: Vec<UcString>,
    parameters: Vec<RfcConnectionParameter>,
    connection: Option<RfcHandle>,
}

impl DynamicRfcApi {
    pub fn load(library: RfcLibrary, parameters: Vec<(String, String)>) -> Result<Self, String> {
        let symbols = RfcSymbols::load(library.library())?;
        let function = FunctionSymbols::load(library.library());
        let width = library.width();
        let mut storage = Vec::with_capacity(parameters.len() * 2);

        // Only the parameter name is ever named in an error: a value may be the password.
        for (name, value) in parameters {
            for text in [&name, &value] {
                storage.push(
                    UcString::encode(width, text)
                        .map_err(|reason| format!("RFC connection parameter {name}: {reason}"))?,
                );
            }
        }

        let parameters = storage
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| RfcConnectionParameter {
                name: pair[0].as_ptr(),
                value: pair[1].as_ptr(),
            })
            .collect();

        Ok(Self {
            _library: library,
            symbols,
            function,
            width,
            _parameter_storage: storage,
            parameters,
            connection: None,
        })
    }

    #[cfg(feature = "test-support")]
    pub fn abi_layout_checked() -> bool {
        use std::mem::{align_of, offset_of, size_of};

        size_of::<RfcConnectionParameter>() == 2 * size_of::<*const c_void>()
            && align_of::<RfcConnectionParameter>() == align_of::<*const c_void>()
            && size_of::<RfcErrorInfo>() == 1752
            && align_of::<RfcErrorInfo>() == 4
            && offset_of!(RfcErrorInfo, group) == 4
            && offset_of!(RfcErrorInfo, key) == 8
            && offset_of!(RfcErrorInfo, message) == 264
            && offset_of!(RfcErrorInfo, abap_msg_class) == 1288
            && offset_of!(RfcErrorInfo, abap_msg_v4) == 1648
            && NARROW_MESSAGE_OFFSET + NARROW_MESSAGE_BYTES
                <= offset_of!(RfcErrorInfo, abap_msg_class)
    }

    pub fn version(&self) -> (u32, u32, u32) {
        let (mut major, mut minor, mut patch) = (0, 0, 0);

        unsafe {
            (self.symbols.get_version)(&mut major, &mut minor, &mut patch);
        }

        (major, minor, patch)
    }

    pub fn ping(&mut self) -> Result<(), TransportError> {
        let connection = self.ensure_open()?;
        let mut error = RfcErrorInfo::default();
        let result = unsafe { (self.symbols.ping)(connection, &mut error) };

        check(self.width, RfcPhase::Open, "RfcPing", result, &error)
    }

    fn ensure_open(&mut self) -> Result<RfcHandle, TransportError> {
        if let Some(connection) = self.connection {
            return Ok(connection);
        }

        let mut error = RfcErrorInfo::default();
        let parameters = if self.parameters.is_empty() {
            std::ptr::null()
        } else {
            self.parameters.as_ptr()
        };
        let connection = unsafe {
            (self.symbols.open_connection)(parameters, self.parameters.len() as c_uint, &mut error)
        };

        if connection.is_null() {
            return Err(rfc_error(
                self.width,
                RfcPhase::Open,
                "RfcOpenConnection",
                &error,
            ));
        }

        self.connection = Some(connection);
        Ok(connection)
    }

    fn structure(
        &self,
        phase: RfcPhase,
        container: RfcHandle,
        name: &str,
    ) -> Result<RfcHandle, TransportError> {
        let name = encode(self.width, phase, name)?;
        let mut value = std::ptr::null_mut();
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.get_structure)(container, name.as_ptr(), &mut value, &mut error)
        };

        check(self.width, phase, "RfcGetStructure", result, &error)?;
        Ok(value)
    }

    fn table(
        &self,
        phase: RfcPhase,
        container: RfcHandle,
        name: &str,
    ) -> Result<RfcHandle, TransportError> {
        let name = encode(self.width, phase, name)?;
        let mut value = std::ptr::null_mut();
        let mut error = RfcErrorInfo::default();
        let result =
            unsafe { (self.symbols.get_table)(container, name.as_ptr(), &mut value, &mut error) };

        check(self.width, phase, "RfcGetTable", result, &error)?;
        Ok(value)
    }

    fn set_string(
        &self,
        container: RfcHandle,
        name: &str,
        value: &str,
    ) -> Result<(), TransportError> {
        let name = encode(self.width, RfcPhase::Prepare, name)?;
        let text = encode(self.width, RfcPhase::Prepare, value)?;
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.set_string)(
                container,
                name.as_ptr(),
                text.as_ptr(),
                text.chars() as c_uint,
                &mut error,
            )
        };

        check(
            self.width,
            RfcPhase::Prepare,
            "RfcSetString",
            result,
            &error,
        )
    }

    fn set_xstring(
        &self,
        container: RfcHandle,
        name: &str,
        value: &[u8],
    ) -> Result<(), TransportError> {
        let name = encode(self.width, RfcPhase::Prepare, name)?;
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.set_xstring)(
                container,
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len() as c_uint,
                &mut error,
            )
        };

        check(
            self.width,
            RfcPhase::Prepare,
            "RfcSetXString",
            result,
            &error,
        )
    }

    fn string_length(
        &self,
        container: RfcHandle,
        name: &UcString,
    ) -> Result<usize, TransportError> {
        let mut length = 0;
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.get_string_length)(container, name.as_ptr(), &mut length, &mut error)
        };

        check(
            self.width,
            RfcPhase::Invoke,
            "RfcGetStringLength",
            result,
            &error,
        )?;
        Ok(length as usize)
    }

    fn get_string(&self, container: RfcHandle, name: &str) -> Result<String, TransportError> {
        let name = encode(self.width, RfcPhase::Invoke, name)?;
        let length = self.string_length(container, &name)?;
        let mut buffer = UcBuffer::new(self.width, length + 1);
        let mut written = 0;
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.get_string)(
                container,
                name.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.chars() as c_uint,
                &mut written,
                &mut error,
            )
        };

        check(self.width, RfcPhase::Invoke, "RfcGetString", result, &error)?;
        buffer
            .text(written as usize)
            .map_err(TransportError::PostSendUncertain)
    }

    fn get_xstring(&self, container: RfcHandle, name: &str) -> Result<Vec<u8>, TransportError> {
        let name = encode(self.width, RfcPhase::Invoke, name)?;
        let length = self.string_length(container, &name)?;
        let mut buffer = vec![0u8; length.max(1)];
        let mut written = 0;
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.get_xstring)(
                container,
                name.as_ptr(),
                buffer.as_mut_ptr().cast(),
                length as c_uint,
                &mut written,
                &mut error,
            )
        };

        check(
            self.width,
            RfcPhase::Invoke,
            "RfcGetXString",
            result,
            &error,
        )?;
        buffer.truncate(written as usize);
        Ok(buffer)
    }

    fn append_row(&self, table: RfcHandle) -> Result<RfcHandle, TransportError> {
        let mut error = RfcErrorInfo::default();
        let row = unsafe { (self.symbols.append_new_row)(table, &mut error) };

        if row.is_null() {
            return Err(rfc_error(
                self.width,
                RfcPhase::Prepare,
                "RfcAppendNewRow",
                &error,
            ));
        }

        Ok(row)
    }

    /// The function description handle, or the error of the lookup. The handle belongs to the
    /// connection's repository and is never destroyed here.
    fn function_descriptor(
        &self,
        connection: RfcHandle,
        name: &UcString,
    ) -> Result<RfcHandle, Box<RfcErrorInfo>> {
        let mut error = RfcErrorInfo::default();
        let descriptor =
            unsafe { (self.symbols.get_function_desc)(connection, name.as_ptr(), &mut error) };

        if descriptor.is_null() {
            return Err(Box::new(error));
        }

        Ok(descriptor)
    }

    fn create_function(
        &self,
        connection: RfcHandle,
        name: &str,
    ) -> Result<RfcHandle, TransportError> {
        let name = encode(self.width, RfcPhase::Prepare, name)?;
        let descriptor = self
            .function_descriptor(connection, &name)
            .map_err(|error| {
                rfc_error(self.width, RfcPhase::Prepare, "RfcGetFunctionDesc", &error)
            })?;
        let mut error = RfcErrorInfo::default();
        let function = unsafe { (self.symbols.create_function)(descriptor, &mut error) };

        if function.is_null() {
            return Err(rfc_error(
                self.width,
                RfcPhase::Prepare,
                "RfcCreateFunction",
                &error,
            ));
        }

        Ok(function)
    }

    fn call(
        &self,
        connection: RfcHandle,
        function: RfcHandle,
        request: &RfcEndpointRequest,
    ) -> Result<RfcEndpointResponse, TransportError> {
        let request_data = self.structure(RfcPhase::Prepare, function, "REQUEST")?;
        let line = self.structure(RfcPhase::Prepare, request_data, "REQUEST_LINE")?;
        self.set_string(line, "METHOD", &request.method)?;
        self.set_string(line, "URI", &request.uri)?;
        self.set_string(line, "VERSION", &request.version)?;
        let headers = self.table(RfcPhase::Prepare, request_data, "HEADER_FIELDS")?;

        for (name, value) in &request.headers {
            let row = self.append_row(headers)?;

            self.set_string(row, "NAME", name)?;
            self.set_string(row, "VALUE", value)?;
        }

        self.set_xstring(request_data, "MESSAGE_BODY", &request.body)?;
        let mut error = RfcErrorInfo::default();
        let result = unsafe { (self.symbols.invoke)(connection, function, &mut error) };

        check(self.width, RfcPhase::Invoke, "RfcInvoke", result, &error)?;
        let response = self.structure(RfcPhase::Invoke, function, "RESPONSE")?;
        let status_line = self.structure(RfcPhase::Invoke, response, "STATUS_LINE")?;
        let status = self
            .get_string(status_line, "STATUS_CODE")?
            .trim()
            .parse::<u16>()
            .map_err(|error| TransportError::PostSendUncertain(error.to_string()))?;
        let body = self.get_xstring(response, "MESSAGE_BODY")?;

        Ok(RfcEndpointResponse {
            status,
            headers: BTreeMap::new(),
            body,
        })
    }
}

impl RfcApi for DynamicRfcApi {
    fn open(&mut self) -> Result<(), TransportError> {
        self.ensure_open().map(|_| ())
    }

    fn invoke(
        &mut self,
        request: RfcEndpointRequest,
    ) -> Result<RfcEndpointResponse, TransportError> {
        let connection = self.ensure_open()?;
        let function = self.create_function(connection, request.function)?;

        let result = self.call(connection, function, &request);
        let mut destroy_error = RfcErrorInfo::default();

        // A failing destroy must not turn a sent request into a reported failure.
        unsafe {
            (self.symbols.destroy_function)(function, &mut destroy_error);
        }

        result
    }

    fn close(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        let mut error = RfcErrorInfo::default();

        unsafe {
            (self.symbols.close_connection)(connection, &mut error);
        }
    }
}

/// The longest text one field, one table cell or one name may carry through the generic call:
/// the widest field of a source interface is a 255-character source line.
pub const MAX_CALL_CHARS: usize = 255;
/// The most rows one table may carry in or out of a generic call.
pub const MAX_CALL_ROWS: usize = 100_000;
/// The most parameters one function description may list.
pub const MAX_CALL_PARAMETERS: usize = 1024;
/// The error key of `RfcGetFunctionDesc` for a function module that does not exist.
pub const FUNCTION_NOT_FOUND_KEY: &str = "FU_NOT_FOUND";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Import,
    Export,
    Changing,
    Tables,
    Other(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterType {
    Char,
    Table,
    Other(i32),
}

/// One parameter of a function description. Lengths are in characters: `nuc_length` for a
/// non-Unicode system, `uc_length` for a Unicode one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParameterInfo {
    pub name: String,
    pub kind: ParameterType,
    pub direction: Direction,
    pub nuc_length: u32,
    pub uc_length: u32,
    pub optional: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionInfo {
    pub name: String,
    pub parameters: Vec<ParameterInfo>,
}

/// A value read back from an exporting field or a table cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Cell {
    Text(String),
    Int(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CellKind {
    /// A character field of this many characters; trailing blanks are dropped.
    Chars(usize),
    /// An INT4 field.
    Int,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColumnRead {
    pub name: String,
    pub kind: CellKind,
}

/// An importing table: one `(field, value)` list per row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableWrite {
    pub name: String,
    pub rows: Vec<Vec<(String, String)>>,
}

/// An exporting table and the columns to read from each row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRead {
    pub name: String,
    pub columns: Vec<ColumnRead>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FunctionCall {
    pub function: String,
    pub imports: Vec<(String, String)>,
    pub tables: Vec<TableWrite>,
    pub exports: Vec<ColumnRead>,
    pub export_tables: Vec<TableRead>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FunctionResult {
    pub exports: Vec<(String, Cell)>,
    pub tables: Vec<(String, Vec<Vec<Cell>>)>,
}

/// The generic function-module primitive: metadata lookup and one invoke. The public CLI and MCP
/// paths never call it: only the transport, the extension module and the factory forwarders name
/// it, and `tests/public_boundary_test.rs` fails when any other source file does.
pub trait FunctionApi {
    /// `None` when the function module does not exist (`FU_NOT_FOUND`); any other failure is an error.
    fn describe_function(&mut self, name: &str) -> Result<Option<FunctionInfo>, TransportError>;

    fn call_function(&mut self, call: &FunctionCall) -> Result<FunctionResult, TransportError>;

    /// Whether the library can serve function calls; `Err` is the library message.
    fn function_support(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A name of a function, parameter or field as the SDK accepts it: at most 30 characters, no blank.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 30 && name.bytes().all(|byte| byte.is_ascii_graphic())
}

fn too_long(what: &str, chars: usize) -> TransportError {
    TransportError::Protocol(format!(
        "RFC call: {what} has {chars} characters; the limit is {MAX_CALL_CHARS}"
    ))
}

/// Checks a call before the first library call, so a bad request never reaches the SDK.
fn validate_call(call: &FunctionCall) -> Result<(), TransportError> {
    let name_error = |name: &str| {
        TransportError::Protocol(format!(
            "RFC call: {name:?} is not a function, parameter or field name"
        ))
    };
    let column_error = |column: &ColumnRead| match column.kind {
        CellKind::Chars(length) if length == 0 || length > MAX_CALL_CHARS => {
            Some(too_long(&format!("column {}", column.name), length))
        }
        _ => None,
    };

    if !valid_name(&call.function) {
        return Err(name_error(&call.function));
    }

    for (name, value) in &call.imports {
        if !valid_name(name) {
            return Err(name_error(name));
        }

        if value.chars().count() > MAX_CALL_CHARS {
            return Err(too_long(&format!("field {name}"), value.chars().count()));
        }
    }

    for table in &call.tables {
        if !valid_name(&table.name) {
            return Err(name_error(&table.name));
        }

        if table.rows.len() > MAX_CALL_ROWS {
            return Err(TransportError::Protocol(format!(
                "RFC call: table {} has {} rows; the limit is {MAX_CALL_ROWS}",
                table.name,
                table.rows.len()
            )));
        }

        for (field, value) in table.rows.iter().flatten() {
            if !valid_name(field) {
                return Err(name_error(field));
            }

            if value.chars().count() > MAX_CALL_CHARS {
                return Err(too_long(
                    &format!("field {field} of table {}", table.name),
                    value.chars().count(),
                ));
            }
        }
    }

    let columns = call
        .exports
        .iter()
        .chain(call.export_tables.iter().flat_map(|table| &table.columns));

    for column in columns {
        if !valid_name(&column.name) {
            return Err(name_error(&column.name));
        }

        if let Some(error) = column_error(column) {
            return Err(error);
        }
    }

    for table in &call.export_tables {
        if !valid_name(&table.name) {
            return Err(name_error(&table.name));
        }
    }

    Ok(())
}

/// Like `rfc_error`, plus the error key (an ABAP exception name, for example) when there is one.
fn function_error(
    width: UcWidth,
    phase: RfcPhase,
    operation: &str,
    error: &RfcErrorInfo,
) -> TransportError {
    let key = error.key(width);

    if key.is_empty() {
        rfc_error(width, phase, operation, error)
    } else {
        rfc_error(width, phase, &format!("{operation} [{key}]"), error)
    }
}

fn function_check(
    width: UcWidth,
    phase: RfcPhase,
    operation: &str,
    result: RfcRc,
    error: &RfcErrorInfo,
) -> Result<(), TransportError> {
    if result == RFC_OK {
        Ok(())
    } else {
        Err(function_error(width, phase, operation, error))
    }
}

/// Byte offsets inside `RFC_PARAMETER_DESC` for a character size and a pointer size, following
/// the field order of `sapnwrfc.h`: `name[31]`, `type`, `direction`, `nucLength`, `ucLength`,
/// `decimals`, `typeDescHandle`, `defaultValue[31]`, `parameterText[80]`, `optional`,
/// `extendedDescription`. A single-byte library lays the same struct out with one-byte characters.
#[derive(Debug, Eq, PartialEq)]
struct ParameterLayout {
    name_bytes: usize,
    kind: usize,
    direction: usize,
    nuc_length: usize,
    uc_length: usize,
    optional: usize,
    size: usize,
}

fn align(offset: usize, to: usize) -> usize {
    offset.div_ceil(to) * to
}

impl ParameterLayout {
    fn new(char_bytes: usize, pointer_bytes: usize) -> Self {
        let name_bytes = 31 * char_bytes;
        let kind = align(name_bytes, 4);
        let (direction, nuc_length, uc_length) = (kind + 4, kind + 8, kind + 12);
        let handle = align(kind + 20, pointer_bytes);
        let default_value = handle + pointer_bytes;
        let text = default_value + 31 * char_bytes;
        let optional = text + 80 * char_bytes;

        Self {
            name_bytes,
            kind,
            direction,
            nuc_length,
            uc_length,
            optional,
            size: align(
                align(optional + 1, pointer_bytes) + pointer_bytes,
                pointer_bytes,
            ),
        }
    }

    fn of(width: UcWidth) -> Self {
        let char_bytes = match width {
            UcWidth::Utf16 => 2,
            UcWidth::Narrow => 1,
        };

        Self::new(char_bytes, std::mem::size_of::<*const c_void>())
    }
}

/// Room for a parameter description of either width.
const PARAMETER_DESC_WORDS: usize = 64;

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

impl DynamicRfcApi {
    /// Whether this library can serve function calls: `Err` names the symbols it does not export.
    /// ADT and SADT never need them.
    pub fn function_support(&self) -> Result<(), String> {
        self.function.as_ref().map(|_| ()).map_err(|missing| {
            format!(
                "the RFC library does not export {}; function module calls are unavailable, ADT and SADT are not affected",
                missing.join(", ")
            )
        })
    }

    fn loaded_symbols(&self) -> Result<FunctionSymbols, TransportError> {
        self.function_support()
            .map(|()| *self.function.as_ref().expect("checked by function_support"))
            .map_err(TransportError::Protocol)
    }

    fn set_chars(
        &self,
        symbols: &FunctionSymbols,
        container: RfcHandle,
        name: &str,
        value: &str,
    ) -> Result<(), TransportError> {
        let name = encode(self.width, RfcPhase::Prepare, name)?;
        let text = encode(self.width, RfcPhase::Prepare, value)?;
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (symbols.set_chars)(
                container,
                name.as_ptr(),
                text.as_ptr(),
                text.chars() as c_uint,
                &mut error,
            )
        };

        function_check(self.width, RfcPhase::Prepare, "RfcSetChars", result, &error)
    }

    fn read_cell(
        &self,
        symbols: &FunctionSymbols,
        container: RfcHandle,
        column: &ColumnRead,
    ) -> Result<Cell, TransportError> {
        let name = encode(self.width, RfcPhase::Invoke, &column.name)?;
        let mut error = RfcErrorInfo::default();

        match column.kind {
            CellKind::Int => {
                let mut value: c_int = 0;
                let result =
                    unsafe { (symbols.get_int)(container, name.as_ptr(), &mut value, &mut error) };

                function_check(self.width, RfcPhase::Invoke, "RfcGetInt", result, &error)?;
                Ok(Cell::Int(value))
            }
            CellKind::Chars(length) => {
                let mut buffer = UcBuffer::new(self.width, length);
                let result = unsafe {
                    (symbols.get_chars)(
                        container,
                        name.as_ptr(),
                        buffer.as_mut_ptr(),
                        length as c_uint,
                        &mut error,
                    )
                };

                function_check(self.width, RfcPhase::Invoke, "RfcGetChars", result, &error)?;
                // The SDK pads a character field with blanks and writes no terminator.
                let text = buffer
                    .text(length)
                    .map_err(TransportError::PostSendUncertain)?;

                Ok(Cell::Text(text.trim_end_matches(' ').to_owned()))
            }
        }
    }

    fn read_table(
        &self,
        symbols: &FunctionSymbols,
        function: RfcHandle,
        read: &TableRead,
    ) -> Result<Vec<Vec<Cell>>, TransportError> {
        let table = self.table(RfcPhase::Invoke, function, &read.name)?;
        let mut count: c_uint = 0;
        let mut error = RfcErrorInfo::default();
        let result = unsafe { (symbols.get_row_count)(table, &mut count, &mut error) };

        function_check(
            self.width,
            RfcPhase::Invoke,
            "RfcGetRowCount",
            result,
            &error,
        )?;

        if count as usize > MAX_CALL_ROWS {
            return Err(TransportError::PostSendUncertain(format!(
                "RFC call: table {} returned {count} rows; the limit is {MAX_CALL_ROWS}",
                read.name
            )));
        }

        let mut rows = Vec::with_capacity(count as usize);

        for index in 0..count {
            let mut error = RfcErrorInfo::default();
            let result = unsafe { (symbols.move_to)(table, index, &mut error) };

            function_check(self.width, RfcPhase::Invoke, "RfcMoveTo", result, &error)?;
            let row = unsafe { (symbols.get_current_row)(table, &mut error) };

            if row.is_null() {
                return Err(function_error(
                    self.width,
                    RfcPhase::Invoke,
                    "RfcGetCurrentRow",
                    &error,
                ));
            }

            rows.push(
                read.columns
                    .iter()
                    .map(|column| self.read_cell(symbols, row, column))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }

        Ok(rows)
    }

    /// Fills the importing fields and tables, invokes, and reads what the call asks for.
    fn run_function(
        &self,
        symbols: &FunctionSymbols,
        connection: RfcHandle,
        function: RfcHandle,
        call: &FunctionCall,
    ) -> Result<FunctionResult, TransportError> {
        for (name, value) in &call.imports {
            self.set_chars(symbols, function, name, value)?;
        }

        for table in &call.tables {
            let handle = self.table(RfcPhase::Prepare, function, &table.name)?;

            for row in &table.rows {
                let row_handle = self.append_row(handle)?;

                for (field, value) in row {
                    self.set_chars(symbols, row_handle, field, value)?;
                }
            }
        }

        let mut error = RfcErrorInfo::default();
        let result = unsafe { (self.symbols.invoke)(connection, function, &mut error) };

        function_check(self.width, RfcPhase::Invoke, "RfcInvoke", result, &error)?;

        let mut outcome = FunctionResult::default();

        for column in &call.exports {
            outcome.exports.push((
                column.name.clone(),
                self.read_cell(symbols, function, column)?,
            ));
        }

        for table in &call.export_tables {
            outcome.tables.push((
                table.name.clone(),
                self.read_table(symbols, function, table)?,
            ));
        }

        Ok(outcome)
    }

    fn parameter(
        &self,
        symbols: &FunctionSymbols,
        descriptor: RfcHandle,
        index: c_uint,
    ) -> Result<ParameterInfo, TransportError> {
        let layout = ParameterLayout::of(self.width);
        // Zeroed and 8-aligned, so the library fills it and every byte read back is initialized.
        let mut raw = [0u64; PARAMETER_DESC_WORDS];
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (symbols.get_parameter_desc_by_index)(
                descriptor,
                index,
                raw.as_mut_ptr().cast(),
                &mut error,
            )
        };

        function_check(
            self.width,
            RfcPhase::Prepare,
            "RfcGetParameterDescByIndex",
            result,
            &error,
        )?;

        debug_assert!(layout.size <= PARAMETER_DESC_WORDS * 8);
        let bytes = raw
            .iter()
            .flat_map(|word| word.to_ne_bytes())
            .collect::<Vec<_>>();
        let name = match self.width {
            UcWidth::Utf16 => from_sap_uc(
                &bytes[..layout.name_bytes]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>(),
            )
            .map_err(TransportError::Protocol)?,
            UcWidth::Narrow => narrow_text(&bytes[..layout.name_bytes]),
        };

        Ok(ParameterInfo {
            name,
            kind: match word(&bytes, layout.kind) as i32 {
                0 => ParameterType::Char,
                5 => ParameterType::Table,
                other => ParameterType::Other(other),
            },
            direction: match word(&bytes, layout.direction) as i32 {
                1 => Direction::Import,
                2 => Direction::Export,
                3 => Direction::Changing,
                7 => Direction::Tables,
                other => Direction::Other(other),
            },
            nuc_length: word(&bytes, layout.nuc_length),
            uc_length: word(&bytes, layout.uc_length),
            optional: bytes[layout.optional] != 0,
        })
    }
}

impl FunctionApi for DynamicRfcApi {
    fn function_support(&self) -> Result<(), String> {
        DynamicRfcApi::function_support(self)
    }

    fn describe_function(&mut self, name: &str) -> Result<Option<FunctionInfo>, TransportError> {
        let symbols = self.loaded_symbols()?;

        if !valid_name(name) {
            return Err(TransportError::Protocol(format!(
                "RFC call: {name:?} is not a function name"
            )));
        }

        let connection = self.ensure_open()?;
        let encoded = encode(self.width, RfcPhase::Prepare, name)?;
        let descriptor = match self.function_descriptor(connection, &encoded) {
            Ok(descriptor) => descriptor,
            Err(error) if error.key(self.width) == FUNCTION_NOT_FOUND_KEY => return Ok(None),
            Err(error) => {
                return Err(function_error(
                    self.width,
                    RfcPhase::Prepare,
                    "RfcGetFunctionDesc",
                    &error,
                ));
            }
        };
        let mut count: c_uint = 0;
        let mut error = RfcErrorInfo::default();
        let result = unsafe { (symbols.get_parameter_count)(descriptor, &mut count, &mut error) };

        function_check(
            self.width,
            RfcPhase::Prepare,
            "RfcGetParameterCount",
            result,
            &error,
        )?;

        if count as usize > MAX_CALL_PARAMETERS {
            return Err(TransportError::Protocol(format!(
                "RFC call: {name} lists {count} parameters; the limit is {MAX_CALL_PARAMETERS}"
            )));
        }

        Ok(Some(FunctionInfo {
            name: name.to_owned(),
            parameters: (0..count)
                .map(|index| self.parameter(&symbols, descriptor, index))
                .collect::<Result<_, _>>()?,
        }))
    }

    fn call_function(&mut self, call: &FunctionCall) -> Result<FunctionResult, TransportError> {
        let symbols = self.loaded_symbols()?;

        validate_call(call)?;
        let connection = self.ensure_open()?;
        let function = self.create_function(connection, &call.function)?;
        let result = self.run_function(&symbols, connection, function, call);
        let mut destroy_error = RfcErrorInfo::default();

        // A failing destroy must not turn a sent request into a reported failure.
        unsafe {
            (self.symbols.destroy_function)(function, &mut destroy_error);
        }

        result
    }
}

impl Drop for DynamicRfcApi {
    fn drop(&mut self) {
        self.close();
    }
}

/// Whether the request is the write a fault targets: the source `PUT`, the activation, the
/// create `POST` or the `DELETE`.
#[cfg(feature = "fault-injection")]
fn is_fault_target(target: crate::fault::Target, request: &Request) -> bool {
    match target {
        crate::fault::Target::Put => is_put_request(request),
        crate::fault::Target::Activate => request.method == "POST" && request.uri == ACTIVATION_URI,
        crate::fault::Target::Create => is_create_post(request),
        crate::fault::Target::Delete => is_delete_request(request),
    }
}

/// What one candidate attempt did: a mapped library that can be used, or why it cannot be.
/// `Loaded` is not that type because it deliberately keeps the preloaded dependencies alive.
pub type Attempt = Result<Library, String>;

/// One candidate the loader may attempt, and where it came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub path: PathBuf,
    pub family: Family,
    pub origin: Origin,
}

impl Candidate {
    /// A path the user named, labelled by the library the path itself names.
    pub fn new(path: PathBuf, origin: Origin) -> Self {
        Self {
            family: family_of(&path),
            path,
            origin,
        }
    }

    /// A path automatic discovery found, labelled with the location family that found it: a SAP GUI
    /// package also ships the plain `libsapnwrfc` library, so the file name alone cannot say which
    /// family a discovered path belongs to.
    pub fn located(path: PathBuf, family: Family, origin: Origin) -> Self {
        Self {
            family,
            path,
            origin,
        }
    }
}

/// Which SAP product provides a library; the doctor reports it, so a run labelled SAP GUI cannot
/// silently load the SDK library.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Family {
    /// The SAP GUI package library, which carries its own RFC implementation.
    Gui,
    /// The NW RFC SDK library.
    Sdk,
}

impl Family {
    /// How the doctor names the family.
    pub fn label(self) -> &'static str {
        match self {
            Self::Gui => "SAP GUI",
            Self::Sdk => "NW RFC SDK",
        }
    }
}

/// Whether the user named the path or automatic discovery found it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// The user named the path, for example `ABAPREACH_RFC_LIBRARY` pointing at a SAP GUI or SDK
    /// library; a failure stops the load instead of falling back.
    Explicit,
    /// Automatic discovery found the path; a failure moves on to the next candidate.
    Discovered,
}

impl Origin {
    /// How the doctor names where the loaded library came from.
    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "explicit ABAPREACH_RFC_LIBRARY",
            Self::Discovered => "discovered",
        }
    }
}

/// The library family a path the user named belongs to: every SAP GUI release ships the `...cm`
/// library, and the SDK ships the plain one, which the documented SAP GUI locations answer too.
/// Automatic discovery does not need this guess, because the location that found a path supplies
/// its family directly.
pub fn family_of(path: &Path) -> Family {
    let text = path.to_string_lossy().to_lowercase();
    let named = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let gui_location = text
        .split(['/', '\\'])
        .any(|component| component.starts_with("sapgui"))
        || text.contains(".app/");

    if named.contains("sapnwrfccm") || gui_location {
        Family::Gui
    } else {
        Family::Sdk
    }
}

/// Why the candidate list produced no library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadError {
    /// A path the user named could not be used; nothing else is attempted.
    Explicit { path: PathBuf, reason: String },
    /// Every discovered candidate failed; each failure is reported with its path.
    Exhausted(Vec<String>),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Explicit { path, reason } => write!(
                formatter,
                "could not load RFC library {}: {reason}; an explicit path does not fall back to \
                 another candidate",
                path.display()
            ),
            Self::Exhausted(errors) => {
                write!(
                    formatter,
                    "could not load RFC library: {}",
                    errors.join("; ")
                )
            }
        }
    }
}

/// The loader decision, computed without mapping anything: the first candidate the attempt accepts
/// wins, a failed discovered candidate is skipped, and a failed explicit path stops the load.
/// `check` must inspect a candidate before it maps it, so a wrong architecture never reaches FFI
/// loading; it returns the accepted candidate's own result, such as the mapped library.
pub fn decide<T>(
    candidates: &[Candidate],
    mut check: impl FnMut(&Candidate) -> Result<T, String>,
) -> Result<(usize, T), LoadError> {
    let mut errors = Vec::new();

    for (index, candidate) in candidates.iter().enumerate() {
        let rejection = match check(candidate) {
            Ok(accepted) => return Ok((index, accepted)),
            Err(reason) => reason,
        };

        if candidate.origin == Origin::Explicit {
            return Err(LoadError::Explicit {
                path: candidate.path.clone(),
                reason: rejection,
            });
        }

        errors.push(format!("{}: {rejection}", candidate.path.display()));
    }

    Err(LoadError::Exhausted(errors))
}

/// The architecture a library image declares, which must match the binary that maps it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arch {
    X86,
    X64,
    Arm32,
    Arm64,
}

impl Arch {
    /// The architecture this binary was built for; `None` when it is not one of the four a
    /// checked RFC library may target, so an unknown host fails closed instead of guessing.
    pub fn current() -> Option<Self> {
        Self::from_target(std::env::consts::ARCH)
    }

    /// The architecture of a Rust target name; the caller decides what an unknown name means.
    pub fn from_target(name: &str) -> Option<Self> {
        match name {
            "x86" => Some(Self::X86),
            "x86_64" => Some(Self::X64),
            "arm" => Some(Self::Arm32),
            "aarch64" => Some(Self::Arm64),
            _ => None,
        }
    }

    /// How a message names the architecture, so a rejection states both sides.
    pub fn label(self) -> &'static str {
        match self {
            Self::X86 => "x86 (32-bit)",
            Self::X64 => "x86-64",
            Self::Arm32 => "arm (32-bit)",
            Self::Arm64 => "arm64",
        }
    }
}

/// The architectures one library image declares: PE and thin Mach-O declare one, ELF one, and a
/// universal Mach-O image more than one. A header this loader cannot read is an error rather than a
/// guess, so an unreadable candidate is never mapped.
pub fn declared_archs(header: &[u8]) -> Result<Vec<Arch>, String> {
    if header.starts_with(&ELF_MAGIC) {
        parse_elf(header).map(|arch| vec![arch])
    } else if header.starts_with(b"MZ") {
        parse_pe(header).map(|arch| vec![arch])
    } else if let Some(image) = macho_magic(header) {
        match image {
            MachO::Thin(cpu_type) => macho_arch(cpu_type).map(|arch| vec![arch]),
            MachO::Universal => parse_macho_universal(header),
        }
    } else {
        Err(format!(
            "unrecognized library header {:02x?}: expected a PE, ELF, or Mach-O image",
            header.get(..4).unwrap_or(header)
        ))
    }
}

fn parse_elf(header: &[u8]) -> Result<Arch, String> {
    let bytes = header.get(..ELF_HEADER_BYTES).ok_or_else(|| {
        format!(
            "truncated ELF header: {} of {ELF_HEADER_BYTES} bytes",
            header.len()
        )
    })?;
    let (class, encoding) = (bytes[4], bytes[5]);
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);

    if encoding != 1 {
        return Err(format!("unsupported ELF data encoding {encoding}"));
    }

    match (class, machine) {
        (1, 0x0003) => Ok(Arch::X86),
        (1, 0x0028) => Ok(Arch::Arm32),
        (2, 0x003e) => Ok(Arch::X64),
        (2, 0x00b7) => Ok(Arch::Arm64),
        _ => Err(format!(
            "unsupported ELF image: class {class}, machine {machine:#06x}"
        )),
    }
}

fn parse_pe(header: &[u8]) -> Result<Arch, String> {
    let offset = header.get(0x3c..PE_OFFSET_END).ok_or_else(|| {
        format!(
            "truncated PE header: {} of {PE_OFFSET_END} bytes",
            header.len()
        )
    })?;
    let pe_offset = u32::from_le_bytes([offset[0], offset[1], offset[2], offset[3]]) as usize;
    let image = header.get(pe_offset..).ok_or_else(|| {
        format!("PE header offset {pe_offset:#x} is outside the inspected region")
    })?;
    let image = image
        .get(..6)
        .ok_or_else(|| format!("truncated PE image at offset {pe_offset:#x}"))?;

    if image[..4] != PE_SIGNATURE {
        return Err(format!("no PE signature at offset {pe_offset:#x}"));
    }

    match u16::from_le_bytes([image[4], image[5]]) {
        0x014c => Ok(Arch::X86),
        0x01c4 => Ok(Arch::Arm32),
        0x8664 => Ok(Arch::X64),
        0xaa64 => Ok(Arch::Arm64),
        machine => Err(format!("unsupported PE machine {machine:#06x}")),
    }
}

/// A Mach-O image kind: a thin image declares one cpu type, a universal one lists its slices.
enum MachO {
    Thin(u32),
    Universal,
}

/// Recognizes a Mach-O image from its stored magic; `None` when the header is not one. Thin
/// images store the magic in either byte order and the cpu type in the same order.
fn macho_magic(header: &[u8]) -> Option<MachO> {
    let magic: [u8; 4] = header.get(..4)?.try_into().ok()?;
    let cpu: [u8; 4] = header.get(4..8)?.try_into().ok()?;

    match magic {
        MACHO_UNIVERSAL => Some(MachO::Universal),
        MACHO_64_BE | MACHO_32_BE => Some(MachO::Thin(u32::from_be_bytes(cpu))),
        MACHO_64_LE | MACHO_32_LE => Some(MachO::Thin(u32::from_le_bytes(cpu))),
        _ => None,
    }
}

fn parse_macho_universal(header: &[u8]) -> Result<Vec<Arch>, String> {
    let count = header
        .get(4..8)
        .map(|count| u32::from_be_bytes([count[0], count[1], count[2], count[3]]) as usize)
        .ok_or_else(|| format!("truncated universal Mach-O header: {} bytes", header.len()))?;

    if count == 0 || count > MAX_MACHO_SLICES {
        return Err(format!(
            "unsupported universal Mach-O header: {count} slices"
        ));
    }

    let entries = header
        .get(8..8 + count * MACHO_FAT_ARCH_BYTES)
        .ok_or_else(|| format!("truncated universal Mach-O header for {count} slices"))?;

    entries
        .chunks(MACHO_FAT_ARCH_BYTES)
        .map(|entry| macho_arch(u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]])))
        .collect()
}

fn macho_arch(cpu_type: u32) -> Result<Arch, String> {
    match cpu_type {
        MACHO_X86 => Ok(Arch::X86),
        MACHO_X86_64 => Ok(Arch::X64),
        MACHO_ARM => Ok(Arch::Arm32),
        MACHO_ARM64 => Ok(Arch::Arm64),
        _ => Err(format!("unsupported Mach-O cpu type {cpu_type:#010x}")),
    }
}

/// Refuses a library whose declared architectures do not include the one this binary was built
/// for. The message names both sides, so a user can tell a 32-bit DLL from a foreign `.so`.
pub fn compatible_arch(expected: Arch, header: &[u8]) -> Result<(), String> {
    let declared = declared_archs(header)?;

    if declared.contains(&expected) {
        Ok(())
    } else {
        let found = declared
            .iter()
            .map(|arch| arch.label())
            .collect::<Vec<_>>()
            .join(" or ");

        Err(format!(
            "architecture mismatch: the library is {found} but this binary is {}",
            expected.label()
        ))
    }
}

/// A mapped library plus the dependencies that were preloaded for it. None of them is ever
/// unloaded: SAP's library leaves thread-local destructors that crash after `dlclose`, and a
/// dependency has to stay mapped for as long as the library that needs it.
pub struct Loaded {
    arch: Arch,
    path: PathBuf,
    library: ManuallyDrop<Library>,
    _dependencies: Vec<ManuallyDrop<Library>>,
    notes: Vec<String>,
}

impl Loaded {
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// The architecture the image header declares, checked before the library was mapped.
    pub fn arch(&self) -> Arch {
        self.arch
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn notes(&self) -> &[String] {
        &self.notes
    }
}

/// The default attempt for one path: inspect the header first, then map the library. A candidate
/// for another architecture is rejected before any FFI call, so a wrong image is never mapped.
pub fn load_compatible(path: &Path) -> Result<Loaded, String> {
    load_candidate(&Candidate::new(path.to_path_buf(), Origin::Discovered))
}

/// One candidate attempt: the architecture check on the image header, the platform's dependency
/// step, and the mapping itself. The caller must keep the preloaded dependencies mapped for as
/// long as the library is used.
pub fn load_candidate(candidate: &Candidate) -> Result<Loaded, String> {
    let expected = Arch::current().ok_or_else(|| {
        format!(
            "unsupported host architecture {}: this build cannot check an RFC library",
            std::env::consts::ARCH
        )
    })?;
    let path = std::path::absolute(&candidate.path).map_err(|error| {
        format!(
            "could not make {} absolute: {error}",
            candidate.path.display()
        )
    })?;
    let header = read_header(&path)?;

    compatible_arch(expected, &header)?;

    let dependencies = platform_dependencies(&path)?;

    map_library(&path).map(|library| Loaded {
        arch: expected,
        path,
        library: ManuallyDrop::new(library),
        _dependencies: dependencies
            .libraries
            .into_iter()
            .map(ManuallyDrop::new)
            .collect(),
        notes: dependencies.notes,
    })
}

/// Linux keeps a library's own folder out of the search path unless the image says `$ORIGIN`, so
/// the siblings an image needs are mapped by absolute path first. macOS resolves them through the
/// install names and preloads nothing.
#[cfg(target_os = "linux")]
fn platform_dependencies(library: &Path) -> Result<PlatformDependencies, String> {
    let file = fs::File::open(library).map_err(|error| {
        format!(
            "could not open {} to read ELF dependencies: {error}",
            library.display()
        )
    })?;
    let dynamic = elf_dynamic(file).map_err(|error| {
        format!(
            "could not read ELF dependencies from {}: {error}",
            library.display()
        )
    })?;
    let directory = library
        .parent()
        .ok_or_else(|| format!("could not read ICU libraries beside {}", library.display()))?;
    let files = fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let plan = dependency_preload(library, Some(&dynamic), &files)?;
    let libraries = preload_dependencies(&plan.paths, map_library)?;

    Ok(PlatformDependencies {
        libraries,
        notes: plan.note.into_iter().collect(),
    })
}

/// Windows resolves a DLL's own static imports from its folder through the load flags, but the
/// SDK opens ICU by bare name later, so those three are mapped first (see
/// `windows_dependency_preload`).
#[cfg(windows)]
fn platform_dependencies(library: &Path) -> Result<PlatformDependencies, String> {
    let files = library
        .parent()
        .and_then(|directory| fs::read_dir(directory).ok())
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let plan = windows_dependency_preload(library, &files);
    let libraries = preload_dependencies(&plan.paths, map_library)?;

    Ok(PlatformDependencies {
        libraries,
        notes: plan.note.into_iter().collect(),
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform_dependencies(_: &Path) -> Result<PlatformDependencies, String> {
    Ok(PlatformDependencies::default())
}

/// Maps a library through the platform loader: `LoadLibraryExW` with the altered search path on
/// Windows, `dlopen` elsewhere.
#[cfg(windows)]
fn map_library(path: &Path) -> Result<Library, String> {
    let flags = windows_load_flags(path);

    unsafe { libloading::os::windows::Library::load_with_flags(path, flags) }
        .map(Into::into)
        .map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn map_library(path: &Path) -> Result<Library, String> {
    unsafe { Library::new(path) }.map_err(|error| error.to_string())
}

const MISSING_ICU_NOTE: &str = "WARNING: RFC ICU libraries were not found beside the RFC library; the NW RFC SDK may still find them through LD_LIBRARY_PATH.";
const MISSING_WINDOWS_ICU_NOTE: &str = "WARNING: RFC ICU libraries were not found beside the RFC library; the NW RFC SDK may still find them through PATH.";
const ICU_PREFIXES: [&str; 3] = ["libicudata", "libicuuc", "libicui18n"];

/// The library paths and doctor note selected before mapping an RFC candidate.
#[derive(Debug, Default)]
pub struct DependencyPreload {
    pub paths: Vec<PathBuf>,
    pub note: Option<String>,
}

#[derive(Default)]
struct PlatformDependencies {
    libraries: Vec<Library>,
    notes: Vec<String>,
}

/// The ICU libraries one SDK image needs when it cannot resolve `$ORIGIN`. A complete highest
/// version set is chosen in the ICU dependency order data, common, internationalization. SAP's
/// RFC library opens these SONAMEs by bare name, so mapping their absolute paths first lets glibc
/// satisfy that later `dlopen` without changing the parent process environment.
pub fn dependency_preload(
    library: &Path,
    dynamic: Option<&ElfDynamic>,
    files: &[PathBuf],
) -> Result<DependencyPreload, String> {
    let dynamic = dynamic.ok_or_else(|| {
        "could not read ELF dynamic section; dependencies cannot be read".to_owned()
    })?;

    if dynamic.declares_origin() {
        return Ok(DependencyPreload::default());
    }

    match icu_dependencies(library, files) {
        Some(paths) => Ok(DependencyPreload { paths, note: None }),
        None => Ok(DependencyPreload {
            paths: Vec::new(),
            note: Some(MISSING_ICU_NOTE.to_owned()),
        }),
    }
}

/// Finds one complete ICU version set in the RFC library's own directory. A file named
/// `libicuuc57.so` has version `57`; the `.so.57` spelling is accepted too. Multiple complete
/// sets are ranked numerically, highest first. Files such as `libsapucum.so` are deliberately
/// ignored because the SDK does not declare or preload them.
pub fn icu_dependencies(library: &Path, files: &[PathBuf]) -> Option<Vec<PathBuf>> {
    icu_set(library, files, icu_version)
}

/// The shared selection: `version_of(file name, prefix)` names the version a file belongs to, and
/// the highest version that has all three files wins. The paths come back data, common,
/// internationalization.
fn icu_set(
    library: &Path,
    files: &[PathBuf],
    version_of: impl Fn(&str, usize) -> Option<String>,
) -> Option<Vec<PathBuf>> {
    let directory = library.parent()?;
    let mut versions = BTreeMap::<String, [Option<PathBuf>; 3]>::new();

    for path in files {
        if path.parent() != Some(directory) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        for index in 0..ICU_PREFIXES.len() {
            let Some(version) = version_of(name, index) else {
                continue;
            };
            let set = versions.entry(version).or_insert([None, None, None]);
            if set[index].as_ref().is_none_or(|current| path < current) {
                set[index] = Some(path.clone());
            }
        }
    }

    versions
        .into_iter()
        .filter_map(|(version, set)| {
            Some((
                version_rank(&version),
                vec![set[0].clone()?, set[1].clone()?, set[2].clone()?],
            ))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, paths)| paths)
}

fn icu_version(name: &str, index: usize) -> Option<String> {
    let suffix = name.strip_prefix(ICU_PREFIXES[index])?;
    let (before_so, after_so) = suffix.split_once(".so")?;
    if !after_so.is_empty() && !after_so.starts_with('.') {
        return None;
    }
    let version = if before_so.is_empty() {
        after_so.strip_prefix('.').unwrap_or_default()
    } else {
        before_so
    };

    (!version.is_empty()).then(|| version.to_owned())
}

/// The Windows file names of the same three libraries, in the same order.
const WINDOWS_ICU_PREFIXES: [&str; 3] = ["icudt", "icuuc", "icuin"];

/// The version of a Windows ICU file: `icuuc57.dll` is version `57`. The match ignores case, as
/// the file system does, and a name with anything but digits between prefix and extension is not
/// an ICU library (`icuuc57d.dll` is a debug build the SDK never loads).
fn windows_icu_version(name: &str, index: usize) -> Option<String> {
    let name = name.to_ascii_lowercase();
    let version = name
        .strip_prefix(WINDOWS_ICU_PREFIXES[index])?
        .strip_suffix(".dll")?;

    (!version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| version.to_owned())
}

/// What the Windows loader maps before an RFC library. The SDK finds its ICU libraries later by
/// bare name (`LoadLibrary("icuuc57.dll")`), and the library's own folder is not on that search
/// path; but a bare name is satisfied by a module of that name that is already mapped, so the
/// complete highest-version set beside the library is mapped first by absolute path, in the ICU
/// dependency order data, common, internationalization. Only the SDK library (`sapnwrfc.dll`) is
/// known to need ICU: the SAP GUI library has none beside it and never asks, so it gets no note.
pub fn windows_dependency_preload(library: &Path, files: &[PathBuf]) -> DependencyPreload {
    // The same rule for the preload and the note: only `sapnwrfc.dll` is known to need ICU, so a
    // stray ICU set beside any other library (the SAP GUI one) is never mapped for it.
    let needs_icu = library
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(rfc_library_filename(Platform::Windows)));

    if !needs_icu {
        return DependencyPreload::default();
    }

    match icu_set(library, files, windows_icu_version) {
        Some(paths) => DependencyPreload { paths, note: None },
        None => DependencyPreload {
            paths: Vec::new(),
            note: Some(MISSING_WINDOWS_ICU_NOTE.to_owned()),
        },
    }
}

fn version_rank(version: &str) -> Vec<u64> {
    version
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// Maps every dependency by absolute path, in order, before the library that needs them. A failure
/// names the path, so the user can tell which sibling is missing; the parent process search path is
/// never modified.
pub fn preload_dependencies<T>(
    dependencies: &[PathBuf],
    mut map: impl FnMut(&Path) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    dependencies
        .iter()
        .map(|path| {
            map(path).map_err(|error| {
                format!("could not preload dependency {}: {error}", path.display())
            })
        })
        .collect()
}

/// The `LoadLibraryExW` flags for one path: the altered search path resolves a DLL's dependencies
/// from its own folder, and the API rejects that flag for a relative path.
pub fn windows_load_flags(path: &Path) -> u32 {
    if is_windows_absolute(path) {
        LOAD_WITH_ALTERED_SEARCH_PATH
    } else {
        0
    }
}

/// Whether Windows reads the path as a full path: a rooted path, a drive-qualified path such as
/// `C:\libraries\sapnwrfc.dll`, or a UNC share such as `\\host\share\sapnwrfc.dll`.
fn is_windows_absolute(path: &Path) -> bool {
    if path.has_root() {
        return true;
    }

    let text = path.to_string_lossy();
    let bytes = text.as_bytes();

    if bytes.len() >= 3 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return matches!(bytes[2], b'\\' | b'/');
    }

    bytes.starts_with(b"\\\\")
}

/// The dependency information of an ELF64 image needed by this loader: only its declared search
/// path. `DT_NEEDED` does not describe the SDK's runtime ICU `dlopen` calls and is ignored.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ElfDynamic {
    run_path: Vec<String>,
}

impl ElfDynamic {
    /// The `DT_RUNPATH` and `DT_RPATH` entries as the image stores them.
    pub fn run_path(&self) -> &[String] {
        &self.run_path
    }

    /// Whether a search path component is `$ORIGIN`, which resolves to the library's own folder; a
    /// library that declares it finds its siblings without help.
    pub fn declares_origin(&self) -> bool {
        self.run_path
            .iter()
            .flat_map(|entry| entry.split(':'))
            .any(|component| {
                let component = component.trim();

                component == ELF_ORIGIN || component.starts_with("$ORIGIN/")
            })
    }
}

/// Reads the dependency information of an ELF64 image without mapping it. An image the loader
/// cannot read is an error, never a guess, so no candidate is preloaded or mapped from it.
pub fn elf_dynamic(mut image: impl Read + Seek) -> Result<ElfDynamic, String> {
    let header = read_at(&mut image, 0, ELF64_HEADER_BYTES as u64)?;

    if !header.starts_with(&ELF_MAGIC) {
        return Err("not an ELF image".to_owned());
    }

    if header[4] != 2 || header[5] != 1 {
        return Err(format!(
            "unsupported ELF image: class {} and data encoding {} (only 64-bit little-endian \
             images are read)",
            header[4], header[5]
        ));
    }

    let table_offset = field_u64(&header, 0x20);
    let table_entry = u64::from(field_u16(&header, 0x36));
    let table_count = u64::from(field_u16(&header, 0x38));

    if table_entry != ELF64_PROGRAM_HEADER_BYTES as u64 {
        return Err(format!("unsupported ELF program header size {table_entry}"));
    }

    let mut loads = Vec::new();
    let mut dynamic = None;

    for index in 0..table_count {
        let offset = table_offset + index * table_entry;
        let entry = read_at(&mut image, offset, ELF64_PROGRAM_HEADER_BYTES as u64)?;

        match field_u32(&entry, PHDR_TYPE) {
            ELF64_LOAD => loads.push(LoadSegment {
                address: field_u64(&entry, PHDR_VADDR),
                size: field_u64(&entry, PHDR_FILESZ),
                offset: field_u64(&entry, PHDR_OFFSET),
            }),
            ELF64_DYNAMIC => {
                dynamic = Some((
                    field_u64(&entry, PHDR_OFFSET),
                    field_u64(&entry, PHDR_FILESZ),
                ))
            }
            _ => {}
        }
    }

    let (offset, filesz) = dynamic.ok_or_else(|| {
        "the image has no dynamic section, so its dependencies cannot be read".to_owned()
    })?;
    let section = read_at(&mut image, offset, filesz)?;
    let mut run_path = Vec::new();
    let (mut string_table, mut string_bytes) = (None, None);

    for entry in section.as_chunks::<ELF64_DYNAMIC_ENTRY_BYTES>().0 {
        let tag = i64::from_le_bytes(entry[..8].try_into().unwrap_or_default());
        let value = u64::from_le_bytes(entry[8..16].try_into().unwrap_or_default());

        match tag {
            DT_NULL => break,
            DT_RPATH | DT_RUNPATH => run_path.push(value),
            DT_STRTAB => string_table = Some(value),
            DT_STRSZ => string_bytes = Some(value),
            _ => {}
        }
    }

    if run_path.is_empty() {
        return Ok(ElfDynamic::default());
    }

    let address = string_table.ok_or_else(|| {
        "the dynamic section names a search path but declares no string table".to_owned()
    })?;
    let bytes = string_bytes.ok_or_else(|| {
        "the dynamic section names a search path but no string table size".to_owned()
    })?;
    let strings = read_at(
        &mut image,
        load_address_offset(&loads, address).ok_or_else(|| {
            format!("string table address {address:#x} is outside every loadable segment")
        })?,
        bytes,
    )?;
    let name = |value: u64| -> Result<String, String> {
        let start = usize::try_from(value)
            .map_err(|_| format!("string offset {value} does not fit the image"))?;
        let tail = strings
            .get(start..)
            .ok_or_else(|| format!("string offset {value} is outside the dynamic string table"))?;
        let end = tail
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(tail.len());

        Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
    };
    let run_path = run_path
        .into_iter()
        .map(name)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ElfDynamic { run_path })
}

/// The largest section this loader reads, so a malformed image cannot ask for an implausible
/// buffer. A section of exactly this length is read, one byte more is refused.
pub const MAX_READ_BYTES: u64 = 1 << 20;

fn read_at(reader: &mut (impl Read + Seek), offset: u64, bytes: u64) -> Result<Vec<u8>, String> {
    if bytes > MAX_READ_BYTES {
        return Err(format!("implausible section length {bytes} at {offset:#x}"));
    }

    let length = usize::try_from(bytes)
        .map_err(|_| format!("implausible section length {bytes} at {offset:#x}"))?;
    let mut buffer = vec![0u8; length];

    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;
    reader
        .read_exact(&mut buffer)
        .map_err(|error| format!("truncated image at {offset:#x}: {error}"))?;

    Ok(buffer)
}

/// One `PT_LOAD` program header used only to locate the run-path string table in the image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoadSegment {
    address: u64,
    size: u64,
    offset: u64,
}

/// The file offset of a loadable address: a dynamic entry stores a load address, and the segment
/// that maps it decides which file bytes those are.
fn load_address_offset(loads: &[LoadSegment], address: u64) -> Option<u64> {
    loads.iter().find_map(|segment| {
        (address >= segment.address && address - segment.address < segment.size)
            .then(|| segment.offset + (address - segment.address))
    })
}

/// One little-endian field of a header or a dynamic entry; a field the buffer cannot hold reads as
/// zero, so a truncated image is reported by the check that needs the value.
fn field_u16(bytes: &[u8], offset: usize) -> u16 {
    field::<2>(bytes, offset)
        .map(u16::from_le_bytes)
        .unwrap_or_default()
}

fn field_u32(bytes: &[u8], offset: usize) -> u32 {
    field::<4>(bytes, offset)
        .map(u32::from_le_bytes)
        .unwrap_or_default()
}

fn field_u64(bytes: &[u8], offset: usize) -> u64 {
    field::<8>(bytes, offset)
        .map(u64::from_le_bytes)
        .unwrap_or_default()
}

fn field<const BYTES: usize>(bytes: &[u8], offset: usize) -> Option<[u8; BYTES]> {
    bytes
        .get(offset..)
        .and_then(|tail| tail.first_chunk::<BYTES>())
        .copied()
}

/// What the doctor reports about a loaded library, so a run labelled SAP GUI cannot pass with the
/// SDK library: path, version, architecture, family, and source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryIdentity {
    path: PathBuf,
    version: String,
    arch: Arch,
    family: Family,
    source: Origin,
    width: UcWidth,
}

impl LibraryIdentity {
    pub fn new(
        path: PathBuf,
        version: String,
        arch: Arch,
        family: Family,
        source: Origin,
        width: UcWidth,
    ) -> Self {
        Self {
            path,
            version,
            arch,
            family,
            source,
            width,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn arch(&self) -> Arch {
        self.arch
    }

    pub fn family(&self) -> Family {
        self.family
    }

    pub fn source(&self) -> Origin {
        self.source
    }

    pub fn width(&self) -> UcWidth {
        self.width
    }

    /// The one line the doctor prints for the library it loaded.
    pub fn report(&self) -> String {
        identity_report(
            &self.path.display().to_string(),
            &self.version,
            self.arch.label(),
            self.family.label(),
            self.source.label(),
            self.width.label(),
        )
    }
}

/// The single doctor line an identity renders; both the loader identity and the doctor-facing one
/// use it, so the six fields always appear in the same order. `chars` is the character size the
/// library reads, `UTF-16` or `single-byte`.
pub fn identity_report(
    path: &str,
    version: &str,
    architecture: &str,
    family: &str,
    source: &str,
    chars: &str,
) -> String {
    format!(
        "{path} version {version} architecture {architecture} family {family} source {source} chars {chars}"
    )
}

fn read_header(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut header = Vec::with_capacity(HEADER_BYTES);

    file.take(HEADER_BYTES as u64)
        .read_to_end(&mut header)
        .map_err(|error| error.to_string())?;

    Ok(header)
}

/// The platform whose library locations are used; tests drive every platform on any host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// The file-system queries candidate discovery needs; tests supply their own fixtures.
pub trait CandidateFs {
    fn is_file(&self, path: &Path) -> bool;

    /// The child directories of `path`, sorted so the search order is deterministic.
    fn child_directories(&self, path: &Path) -> Vec<PathBuf>;
}

/// The real file system behind `Env::current`; tests pair it with a temporary directory to prove
/// the discovery code path against actual files.
pub struct SystemFs;

impl CandidateFs for SystemFs {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn child_directories(&self, path: &Path) -> Vec<PathBuf> {
        let mut directories = fs::read_dir(path)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        directories.sort();
        directories
    }
}

/// One platform's SAP GUI candidates plus the decisions the doctor has to report.
struct GuiDiscovery {
    candidates: Vec<PathBuf>,
    notes: Vec<String>,
}

/// The NW RFC SDK library name per platform.
pub fn rfc_library_filename(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "sapnwrfc.dll",
        Platform::MacOs => "libsapnwrfc.dylib",
        Platform::Linux => "libsapnwrfc.so",
    }
}

/// The SAP GUI library name per platform.
pub fn gui_library_filename(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "sapnwrfccm.dll",
        Platform::MacOs => "libsapnwrfccm.dylib",
        Platform::Linux => "libsapnwrfccm.so",
    }
}

/// The SDK root order: `SAPNWRFC_HOME`, the home directory, then the platform standard roots.
/// A missing root stays in the list; the loader reports why that path could not be loaded.
pub fn sdk_candidates(platform: Platform, env: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(home) = non_empty(env, "SAPNWRFC_HOME") {
        roots.push(PathBuf::from(home));
    }

    if let Some(home) = home_directory(env) {
        roots.push(home.join("sap").join("nwrfcsdk"));
    }

    match platform {
        Platform::Windows => roots.push(program_files(env).join("SAP").join("nwrfcsdk")),
        Platform::MacOs | Platform::Linux => roots.extend(
            [UNIX_SDK_ROOT, UNIX_LOCAL_SDK_ROOT]
                .into_iter()
                .map(PathBuf::from),
        ),
    }

    roots
        .into_iter()
        .map(|root| root.join("lib").join(rfc_library_filename(platform)))
        .collect()
}

fn gui_candidates(
    platform: Platform,
    env: &BTreeMap<String, String>,
    fs: &dyn CandidateFs,
) -> GuiDiscovery {
    match platform {
        Platform::Windows => windows_gui(env, fs),
        Platform::MacOs => macos_gui(fs),
        Platform::Linux => linux_gui(env, fs),
    }
}

/// Windows: the 64-bit SAP GUI directory only; this binary cannot load the 32-bit library.
fn windows_gui(env: &BTreeMap<String, String>, fs: &dyn CandidateFs) -> GuiDiscovery {
    let library = |program_files: PathBuf| {
        program_files
            .join("SAP")
            .join("FrontEnd")
            .join("SAPGUI")
            .join(gui_library_filename(Platform::Windows))
    };
    let gui = library(program_files(env));
    let gui_32_bit = library(program_files_x86(env));
    let mut candidates = Vec::new();
    let mut notes = Vec::new();

    if fs.is_file(&gui) {
        candidates.push(gui);
    }

    if fs.is_file(&gui_32_bit) {
        notes.push(format!(
            "RFC SAP GUI 32-bit: skipped {} (a 32-bit library cannot load into the 64-bit process)",
            gui_32_bit.display()
        ));
    }

    GuiDiscovery { candidates, notes }
}

/// macOS: every bundle below `/Applications`, highest version first and never loaded to rank it.
fn macos_gui(fs: &dyn CandidateFs) -> GuiDiscovery {
    let mut bundles = Vec::new();

    collect_gui_bundles(
        Path::new(MACOS_APPLICATIONS),
        MAX_GUI_SEARCH_DEPTH,
        fs,
        &mut bundles,
    );

    let mut ranked = bundles
        .into_iter()
        .map(|bundle| (bundle_version(&bundle), bundle))
        .collect::<Vec<_>>();

    ranked.sort_by(|(left_version, left), (right_version, right)| {
        right_version
            .cmp(left_version)
            .then_with(|| left.cmp(right))
    });

    let candidates = ranked
        .into_iter()
        .flat_map(|(_, bundle)| macos_bundle_libraries(&bundle))
        .filter(|path| fs.is_file(path))
        .collect();

    GuiDiscovery {
        candidates,
        notes: Vec::new(),
    }
}

fn macos_bundle_libraries(bundle: &Path) -> [PathBuf; 2] {
    let bin = bundle.join("Contents").join("Resources").join("bin");

    [
        bin.join("unicode").join("libsapnwrfc.dylib"),
        bin.join("libsapnwrfccm.dylib"),
    ]
}

/// A bundle is a directory that holds either SAP GUI library; its own subtree is not searched.
fn collect_gui_bundles(
    root: &Path,
    depth: usize,
    fs: &dyn CandidateFs,
    bundles: &mut Vec<PathBuf>,
) {
    if macos_bundle_libraries(root)
        .iter()
        .any(|path| fs.is_file(path))
    {
        bundles.push(root.to_path_buf());

        return;
    }

    if depth == 0 {
        return;
    }

    for directory in fs.child_directories(root) {
        collect_gui_bundles(&directory, depth - 1, fs, bundles);
    }
}

/// The first version number in the bundle name, e.g. `SAP GUI for Java 8.10 rev15.app` -> [8, 10].
/// The ranking reads the name only, so no candidate library is mapped just to learn its version.
pub fn bundle_version(bundle: &Path) -> Vec<u32> {
    let Some(name) = bundle.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };

    name.split(|character: char| !character.is_ascii_digit() && character != '.')
        .find_map(|token| {
            token
                .trim_matches('.')
                .split('.')
                .map(str::parse)
                .collect::<Result<Vec<u32>, _>>()
                .ok()
        })
        .unwrap_or_default()
}

/// Linux: the SAP GUI for Java package first (direct layout, then `SAPGUI<version>` installs
/// highest first, per root), then the SDK family; ICU stays in `unicode/`.
fn linux_gui(env: &BTreeMap<String, String>, fs: &dyn CandidateFs) -> GuiDiscovery {
    let mut roots = Vec::new();

    if let Some(home) = home_directory(env) {
        roots.push(home.join("sapgui"));
    }

    roots.extend(
        [LINUX_GUI_ROOT, LINUX_LOCAL_GUI_ROOT]
            .into_iter()
            .map(PathBuf::from),
    );

    let candidates = roots
        .into_iter()
        .flat_map(|root| {
            let mut installs = vec![root.clone()];

            installs.extend(linux_versioned_installs(&root, fs));

            installs
        })
        .flat_map(|install| linux_install_libraries(&install))
        .filter(|path| fs.is_file(path))
        .collect();

    GuiDiscovery {
        candidates,
        notes: Vec::new(),
    }
}

fn linux_install_libraries(install: &Path) -> [PathBuf; 2] {
    let bin = install.join("bin");

    [
        bin.join("unicode")
            .join(rfc_library_filename(Platform::Linux)),
        bin.join(gui_library_filename(Platform::Linux)),
    ]
}

/// The direct child directories named like `SAPGUI8.10rev14`, highest version first. A sibling
/// without a version, such as the launcher-only `SAPGUI`, is not an install.
fn linux_versioned_installs(root: &Path, fs: &dyn CandidateFs) -> Vec<PathBuf> {
    let mut ranked = fs
        .child_directories(root)
        .into_iter()
        .filter_map(|directory| {
            let name = directory.file_name()?.to_str()?;
            let version = install_version(name.strip_prefix("SAPGUI")?);

            (!version.is_empty()).then_some((version, directory))
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|(left_version, left), (right_version, right)| {
        right_version
            .cmp(left_version)
            .then_with(|| left.cmp(right))
    });

    ranked.into_iter().map(|(_, directory)| directory).collect()
}

/// Every number in the install name, e.g. `8.10rev14` -> [8, 10, 14], so the revision breaks a tie.
/// Vector order compares major, minor, then revision numerically.
pub fn install_version(name: &str) -> Vec<u32> {
    name.split(|character: char| !character.is_ascii_digit())
        .filter(|token| !token.is_empty())
        .filter_map(|token| token.parse().ok())
        .collect()
}

fn home_directory(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|key| non_empty(env, key).map(PathBuf::from))
}

fn program_files(env: &BTreeMap<String, String>) -> PathBuf {
    non_empty(env, "ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

fn program_files_x86(env: &BTreeMap<String, String>) -> PathBuf {
    non_empty(env, "ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
}

fn non_empty<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

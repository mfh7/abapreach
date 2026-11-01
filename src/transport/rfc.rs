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

/// The library reads this when it is mapped, before any symbol can be called.
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
/// `DT_NULL`, `DT_NEEDED`, `DT_STRTAB`, `DT_STRSZ`, `DT_RPATH`, and `DT_RUNPATH` in an ELF64 image.
const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
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
const PROGRAM_URI_PREFIX: &str = "/sap/bc/adt/programs/programs/";
const CHECKRUNS_URI: &str = "/sap/bc/adt/checkruns?reporters=abapCheckRun";
const ACTIVATION_URI: &str = "/sap/bc/adt/activation?method=activate&preauditRequested=true";
const RFC_OK: c_int = 0;
const RFC_COMMUNICATION_FAILURE: c_int = 1;
const RFC_LOGON_FAILURE: c_int = 2;
const RFC_ABAP_RUNTIME_FAILURE: c_int = 3;
const RFC_ABAP_MESSAGE: c_int = 4;
const RFC_ABAP_EXCEPTION: c_int = 5;
const RFC_ABAP_CLASS_EXCEPTION: c_int = 27;
const RFC_AUTHORIZATION_FAILURE: c_int = 29;

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

        point_trace_at_temp_dir();

        let (chosen, loaded) =
            decide(&candidates, load_candidate).map_err(|error| error.to_string())?;

        set_trace_directory(loaded.library());

        Ok(Self {
            path: candidates[chosen].path.clone(),
            arch: loaded.arch(),
            family: candidates[chosen].family,
            source: candidates[chosen].origin,
            loaded,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
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
        ))
    }

    pub fn library(&self) -> &Library {
        self.loaded.library()
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

/// Sets that variable: the library reads it when it is loaded, before any symbol can be called.
fn point_trace_at_temp_dir() {
    let (name, directory) = trace_environment();

    // Not thread safe by contract; the process is single threaded until the library is mapped.
    unsafe { std::env::set_var(name, directory) };
}

/// `RfcSetTraceDir` is optional: not every SDK or GUI release exports it.
fn set_trace_directory(library: &Library) {
    let set_trace_dir = unsafe { library.get::<RfcSetTraceDir>(b"RfcSetTraceDir\0") }
        .ok()
        .map(|symbol| *symbol);

    apply_trace_directory(set_trace_dir);
}

/// Calls `RfcSetTraceDir` when the library exports it, otherwise the environment set before the
/// load stays in charge, so `dev_rfc.log` never lands in the working directory either way.
pub fn apply_trace_directory(set_trace_dir: Option<RfcSetTraceDir>) -> TraceTarget {
    let Some(set_trace_dir) = set_trace_dir else {
        return TraceTarget::Environment;
    };

    let directory = sap_uc(&trace_directory().display().to_string());
    let mut error = RfcErrorInfo::default();

    // A failing call only leaves the environment as the trace setting.
    unsafe { set_trace_dir(directory.as_ptr(), (&raw mut error).cast::<c_void>()) };

    TraceTarget::Symbol
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
    /// Test-only: the write whose reply is discarded after SAP executed it.
    #[cfg(feature = "fault-injection")]
    fault: Option<crate::fault::Fault>,
}

impl<A> RfcTransport<A> {
    pub fn new(api: A) -> Self {
        Self {
            api,
            opened: false,
            #[cfg(feature = "fault-injection")]
            fault: crate::fault::Fault::from_env(),
        }
    }

    #[cfg(feature = "fault-injection")]
    pub fn with_fault(api: A, fault: Option<crate::fault::Fault>) -> Self {
        Self {
            api,
            opened: false,
            fault,
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
        if !is_allowed(&request) {
            return Err(TransportError::RequestNotAllowed(format!(
                "{} {}",
                request.method, request.uri
            )));
        }

        self.ensure_open()?;
        // Decided before the request is consumed by the call.
        #[cfg(feature = "fault-injection")]
        let dropped = self
            .fault
            .and_then(|fault| fault.drop_reply(is_fault_target(fault.target(), &request)));
        let response = self.api.invoke(RfcEndpointRequest {
            function: SADT_REST_RFC_ENDPOINT,
            method: request.method,
            uri: request.uri,
            version: "HTTP/1.1".to_owned(),
            headers: request.headers,
            body: request.body,
        })?;

        // The RFC call has run and SAP has executed the write; only the reply is discarded.
        #[cfg(feature = "fault-injection")]
        if let Some(error) = dropped {
            return Err(error);
        }

        Ok(Response {
            status: response.status,
            headers: response.headers,
            body: response.body,
        })
    }

    fn close(&mut self) -> Result<(), TransportError> {
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
}

#[cfg(feature = "test-support")]
impl FakeRfcApi {
    pub fn ok() -> Self {
        Self {
            opens: 0,
            closes: 0,
            invocations: 0,
            last_request: None,
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

        Ok(RfcEndpointResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Vec::new(),
        })
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
pub fn connection_parameters(connection: &Connection) -> Vec<(String, String)> {
    let mut parameters = vec![
        ("ASHOST".to_owned(), connection.rfc.ashost.clone()),
        ("SYSNR".to_owned(), connection.rfc.sysnr.clone()),
        ("CLIENT".to_owned(), connection.client.clone()),
        ("USER".to_owned(), connection.user.clone()),
        ("PASSWD".to_owned(), connection.password.clone()),
        ("LANG".to_owned(), connection.lang.clone()),
    ];

    if let Some(router) = &connection.rfc.saprouter {
        parameters.push(("SAPROUTER".to_owned(), router.clone()));
    }

    parameters
}

#[repr(C)]
struct RfcConnectionParameter {
    name: *const SapUc,
    value: *const SapUc,
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
type RfcGetVersion = unsafe extern "C" fn(*mut c_uint, *mut c_uint, *mut c_uint) -> *const SapUc;
type RfcGetFunctionDesc =
    unsafe extern "C" fn(RfcHandle, *const SapUc, *mut RfcErrorInfo) -> RfcHandle;
type RfcCreateFunction = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcHandle;
type RfcGetStructure =
    unsafe extern "C" fn(RfcHandle, *const SapUc, *mut RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcGetTable =
    unsafe extern "C" fn(RfcHandle, *const SapUc, *mut RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcAppendNewRow = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcHandle;
type RfcSetString =
    unsafe extern "C" fn(RfcHandle, *const SapUc, *const SapUc, c_uint, *mut RfcErrorInfo) -> RfcRc;
type RfcSetXString = unsafe extern "C" fn(
    RfcHandle,
    *const SapUc,
    *const c_void,
    c_uint,
    *mut RfcErrorInfo,
) -> RfcRc;
type RfcGetStringLength =
    unsafe extern "C" fn(RfcHandle, *const SapUc, *mut c_uint, *mut RfcErrorInfo) -> RfcRc;
type RfcGetString = unsafe extern "C" fn(
    RfcHandle,
    *const SapUc,
    *mut SapUc,
    c_uint,
    *mut c_uint,
    *mut RfcErrorInfo,
) -> RfcRc;
type RfcGetXString = unsafe extern "C" fn(
    RfcHandle,
    *const SapUc,
    *mut c_void,
    c_uint,
    *mut c_uint,
    *mut RfcErrorInfo,
) -> RfcRc;
type RfcInvoke = unsafe extern "C" fn(RfcHandle, RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcDestroyFunction = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcCloseConnection = unsafe extern "C" fn(RfcHandle, *mut RfcErrorInfo) -> RfcRc;
type RfcSetTraceDir = unsafe extern "C" fn(*const SapUc, *mut c_void) -> RfcRc;

/// Where the trace directory was set; the environment is the fallback when the symbol is absent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceTarget {
    Symbol,
    Environment,
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

fn rfc_error(phase: RfcPhase, operation: &str, error: &RfcErrorInfo) -> TransportError {
    let message =
        from_sap_uc(&error.message).unwrap_or_else(|_| "unreadable error text".to_owned());

    classify_rfc_error(phase, error.code, &format!("{operation}: {message}"))
}

fn check(
    phase: RfcPhase,
    operation: &str,
    result: RfcRc,
    error: &RfcErrorInfo,
) -> Result<(), TransportError> {
    if result == RFC_OK {
        Ok(())
    } else {
        Err(rfc_error(phase, operation, error))
    }
}

pub struct DynamicRfcApi {
    // Keeps the library mapped for as long as the copied function pointers are used.
    _library: RfcLibrary,
    symbols: RfcSymbols,
    // Owns the UTF-16 buffers that `parameters` points into.
    _parameter_storage: Vec<Vec<SapUc>>,
    parameters: Vec<RfcConnectionParameter>,
    connection: Option<RfcHandle>,
}

impl DynamicRfcApi {
    pub fn load(library: RfcLibrary, parameters: Vec<(String, String)>) -> Result<Self, String> {
        let symbols = RfcSymbols::load(library.library())?;
        let storage = parameters
            .into_iter()
            .flat_map(|(name, value)| [sap_uc(&name), sap_uc(&value)])
            .collect::<Vec<_>>();
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
            _parameter_storage: storage,
            parameters,
            connection: None,
        })
    }

    #[cfg(feature = "test-support")]
    pub fn abi_layout_checked() -> bool {
        use std::mem::{align_of, offset_of, size_of};

        size_of::<RfcConnectionParameter>() == 2 * size_of::<*const SapUc>()
            && align_of::<RfcConnectionParameter>() == align_of::<*const SapUc>()
            && size_of::<RfcErrorInfo>() == 1752
            && align_of::<RfcErrorInfo>() == 4
            && offset_of!(RfcErrorInfo, group) == 4
            && offset_of!(RfcErrorInfo, key) == 8
            && offset_of!(RfcErrorInfo, message) == 264
            && offset_of!(RfcErrorInfo, abap_msg_class) == 1288
            && offset_of!(RfcErrorInfo, abap_msg_v4) == 1648
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

        check(RfcPhase::Open, "RfcPing", result, &error)
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
            return Err(rfc_error(RfcPhase::Open, "RfcOpenConnection", &error));
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
        let name = sap_uc(name);
        let mut value = std::ptr::null_mut();
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.get_structure)(container, name.as_ptr(), &mut value, &mut error)
        };

        check(phase, "RfcGetStructure", result, &error)?;
        Ok(value)
    }

    fn table(&self, container: RfcHandle, name: &str) -> Result<RfcHandle, TransportError> {
        let name = sap_uc(name);
        let mut value = std::ptr::null_mut();
        let mut error = RfcErrorInfo::default();
        let result =
            unsafe { (self.symbols.get_table)(container, name.as_ptr(), &mut value, &mut error) };

        check(RfcPhase::Prepare, "RfcGetTable", result, &error)?;
        Ok(value)
    }

    fn set_string(
        &self,
        container: RfcHandle,
        name: &str,
        value: &str,
    ) -> Result<(), TransportError> {
        let name = sap_uc(name);
        let units = sap_uc(value);
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.set_string)(
                container,
                name.as_ptr(),
                units.as_ptr(),
                (units.len() - 1) as c_uint,
                &mut error,
            )
        };

        check(RfcPhase::Prepare, "RfcSetString", result, &error)
    }

    fn set_xstring(
        &self,
        container: RfcHandle,
        name: &str,
        value: &[u8],
    ) -> Result<(), TransportError> {
        let name = sap_uc(name);
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

        check(RfcPhase::Prepare, "RfcSetXString", result, &error)
    }

    fn string_length(&self, container: RfcHandle, name: &[SapUc]) -> Result<usize, TransportError> {
        let mut length = 0;
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.get_string_length)(container, name.as_ptr(), &mut length, &mut error)
        };

        check(RfcPhase::Invoke, "RfcGetStringLength", result, &error)?;
        Ok(length as usize)
    }

    fn get_string(&self, container: RfcHandle, name: &str) -> Result<String, TransportError> {
        let name = sap_uc(name);
        let length = self.string_length(container, &name)?;
        let mut buffer = vec![0; length + 1];
        let mut written = 0;
        let mut error = RfcErrorInfo::default();
        let result = unsafe {
            (self.symbols.get_string)(
                container,
                name.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as c_uint,
                &mut written,
                &mut error,
            )
        };

        check(RfcPhase::Invoke, "RfcGetString", result, &error)?;
        from_sap_uc(&buffer[..written as usize]).map_err(TransportError::PostSendUncertain)
    }

    fn get_xstring(&self, container: RfcHandle, name: &str) -> Result<Vec<u8>, TransportError> {
        let name = sap_uc(name);
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

        check(RfcPhase::Invoke, "RfcGetXString", result, &error)?;
        buffer.truncate(written as usize);
        Ok(buffer)
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
        let headers = self.table(request_data, "HEADER_FIELDS")?;

        for (name, value) in &request.headers {
            let mut error = RfcErrorInfo::default();
            let row = unsafe { (self.symbols.append_new_row)(headers, &mut error) };

            if row.is_null() {
                return Err(rfc_error(RfcPhase::Prepare, "RfcAppendNewRow", &error));
            }

            self.set_string(row, "NAME", name)?;
            self.set_string(row, "VALUE", value)?;
        }

        self.set_xstring(request_data, "MESSAGE_BODY", &request.body)?;
        let mut error = RfcErrorInfo::default();
        let result = unsafe { (self.symbols.invoke)(connection, function, &mut error) };

        check(RfcPhase::Invoke, "RfcInvoke", result, &error)?;
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
        let name = sap_uc(request.function);
        let mut error = RfcErrorInfo::default();
        let descriptor =
            unsafe { (self.symbols.get_function_desc)(connection, name.as_ptr(), &mut error) };

        if descriptor.is_null() {
            return Err(rfc_error(RfcPhase::Prepare, "RfcGetFunctionDesc", &error));
        }

        let function = unsafe { (self.symbols.create_function)(descriptor, &mut error) };

        if function.is_null() {
            return Err(rfc_error(RfcPhase::Prepare, "RfcCreateFunction", &error));
        }

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

impl Drop for DynamicRfcApi {
    fn drop(&mut self) {
        self.close();
    }
}

fn is_allowed(request: &Request) -> bool {
    match request.method.as_str() {
        "GET" => {
            request.uri == "/sap/bc/adt/discovery"
                || is_source_uri(&request.uri, "version=active")
                || is_source_uri(&request.uri, "version=inactive")
                || is_versions_uri(&request.uri)
        }
        "POST" => {
            is_lock_uri(&request.uri)
                || is_unlock_uri(&request.uri)
                || request.uri == CHECKRUNS_URI
                || request.uri == ACTIVATION_URI
        }
        "PUT" => is_put_uri(&request.uri),
        _ => false,
    }
}

/// Whether the request is the write a fault targets: the source `PUT` or the activation.
#[cfg(feature = "fault-injection")]
fn is_fault_target(target: crate::fault::Target, request: &Request) -> bool {
    match target {
        crate::fault::Target::Put => request.method == "PUT" && is_put_uri(&request.uri),
        crate::fault::Target::Activate => request.method == "POST" && request.uri == ACTIVATION_URI,
    }
}

fn program_and_query(uri: &str) -> Option<(&str, &str)> {
    let (program, query) = uri.strip_prefix(PROGRAM_URI_PREFIX)?.split_once('?')?;

    if program.is_empty() || program.contains('/') {
        return None;
    }

    Some((program, query))
}

fn source_and_query(uri: &str) -> Option<(&str, &str)> {
    let (source, query) = uri.strip_prefix(PROGRAM_URI_PREFIX)?.split_once('?')?;
    let program = source.strip_suffix("/source/main")?;

    if program.is_empty() || program.contains('/') {
        return None;
    }

    Some((program, query))
}

fn is_source_uri(uri: &str, expected_query: &str) -> bool {
    let Some((_, query)) = source_and_query(uri) else {
        return false;
    };

    query == expected_query
}

/// `GET <program>/source/main/versions`: the source version list the `foreign_inactive` guard
/// reads.
fn is_versions_uri(uri: &str) -> bool {
    let Some(program) = uri
        .strip_prefix(PROGRAM_URI_PREFIX)
        .and_then(|rest| rest.strip_suffix("/source/main/versions"))
    else {
        return false;
    };

    !program.is_empty() && !program.contains('/')
}

fn is_lock_uri(uri: &str) -> bool {
    matches!(
        program_and_query(uri),
        Some((_, "_action=LOCK&accessMode=MODIFY"))
    )
}

fn is_unlock_uri(uri: &str) -> bool {
    let Some((_, query)) = program_and_query(uri) else {
        return false;
    };

    query
        .strip_prefix("_action=UNLOCK&lockHandle=")
        .is_some_and(|handle| !handle.is_empty() && !handle.contains('&'))
}

fn is_put_uri(uri: &str) -> bool {
    let Some((_, query)) = source_and_query(uri) else {
        return false;
    };
    let Some(lock_and_corr) = query.strip_prefix("lockHandle=") else {
        return false;
    };

    match lock_and_corr.split_once("&corrNr=") {
        Some((handle, corr_nr)) => {
            !handle.is_empty()
                && !handle.contains('&')
                && !corr_nr.is_empty()
                && !corr_nr.contains('&')
        }
        None => !lock_and_corr.is_empty() && !lock_and_corr.contains('&'),
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
    library: ManuallyDrop<Library>,
    _dependencies: Vec<ManuallyDrop<Library>>,
}

impl Loaded {
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// The architecture the image header declares, checked before the library was mapped.
    pub fn arch(&self) -> Arch {
        self.arch
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
    let header = read_header(&candidate.path)?;

    compatible_arch(expected, &header)?;

    let dependencies = platform_dependencies(candidate)?;

    map_library(&candidate.path).map(|library| Loaded {
        arch: expected,
        library: ManuallyDrop::new(library),
        _dependencies: dependencies.into_iter().map(ManuallyDrop::new).collect(),
    })
}

/// Linux keeps a library's own folder out of the search path unless the image says `$ORIGIN`, so
/// the siblings an image needs are mapped by absolute path first. Windows resolves a DLL's own
/// dependencies from its folder through the load flags, and macOS resolves them through the
/// install names, so neither platform preloads anything.
#[cfg(target_os = "linux")]
fn platform_dependencies(candidate: &Candidate) -> Result<Vec<Library>, String> {
    let dynamic = fs::File::open(&candidate.path)
        .ok()
        .and_then(|file| elf_dynamic(file).ok());
    let dependencies = dependency_preload(
        &candidate.path,
        candidate.family,
        dynamic.as_ref(),
        &SystemFs,
    );

    preload_dependencies(&dependencies, |path| map_library(path))
}

#[cfg(not(target_os = "linux"))]
fn platform_dependencies(_: &Candidate) -> Result<Vec<Library>, String> {
    Ok(Vec::new())
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

/// Whether a library needs its siblings preloaded: the SDK family never declares `$ORIGIN`, and any
/// other library that does not declare it needs the same treatment.
pub fn needs_dependency_preload(family: Family, declares_origin: bool) -> bool {
    family == Family::Sdk || !declares_origin
}

/// The siblings one library has to preload, in its own dependency order: the dependency names it
/// declares, the ones that sit beside it in its own folder, and never the library itself.
pub fn sibling_dependencies(
    library: &Path,
    needed: &[String],
    fs: &dyn CandidateFs,
) -> Vec<PathBuf> {
    let Some(directory) = library.parent() else {
        return Vec::new();
    };

    needed
        .iter()
        .map(|name| directory.join(name))
        .filter(|path| path != library && fs.is_file(path))
        .collect()
}

/// The dependencies to preload for one library: the origin rule selects whether the sibling step
/// runs at all, and an image whose dynamic section could not be read leaves nothing to preload.
pub fn dependency_preload(
    library: &Path,
    family: Family,
    dynamic: Option<&ElfDynamic>,
    fs: &dyn CandidateFs,
) -> Vec<PathBuf> {
    let Some(dynamic) = dynamic else {
        return Vec::new();
    };

    if !needs_dependency_preload(family, dynamic.declares_origin()) {
        return Vec::new();
    }

    sibling_dependencies(library, dynamic.needed(), fs)
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

/// The dependency information of an ELF64 image: the libraries it needs and the search path it
/// declares for them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ElfDynamic {
    needed: Vec<String>,
    run_path: Vec<String>,
}

impl ElfDynamic {
    /// The `DT_NEEDED` entries, in the image's own order.
    pub fn needed(&self) -> &[String] {
        &self.needed
    }

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
    let mut needed = Vec::new();
    let mut run_path = Vec::new();
    let (mut string_table, mut string_bytes) = (None, None);

    for entry in section.as_chunks::<ELF64_DYNAMIC_ENTRY_BYTES>().0 {
        let tag = i64::from_le_bytes(entry[..8].try_into().unwrap_or_default());
        let value = u64::from_le_bytes(entry[8..16].try_into().unwrap_or_default());

        match tag {
            DT_NULL => break,
            DT_NEEDED => needed.push(value),
            DT_RPATH | DT_RUNPATH => run_path.push(value),
            DT_STRTAB => string_table = Some(value),
            DT_STRSZ => string_bytes = Some(value),
            _ => {}
        }
    }

    if needed.is_empty() && run_path.is_empty() {
        return Ok(ElfDynamic::default());
    }

    let address = string_table.ok_or_else(|| {
        "the dynamic section names dependencies but declares no string table".to_owned()
    })?;
    let bytes = string_bytes.ok_or_else(|| {
        "the dynamic section declares a string table but no size for it".to_owned()
    })?;
    let strings = read_at(
        &mut image,
        load_address_offset(&loads, address).ok_or_else(|| {
            format!("string table address {address:#x} is outside every loadable segment")
        })?,
        bytes,
    )?;
    let mut name = |value: u64| -> Result<String, String> {
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
    let needed = needed
        .into_iter()
        .map(&mut name)
        .collect::<Result<Vec<_>, _>>()?;
    let run_path = run_path
        .into_iter()
        .map(name)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ElfDynamic { needed, run_path })
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

/// One `PT_LOAD` program header: the address it starts at, how many bytes it maps, and where in the
/// file those bytes start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadSegment {
    pub address: u64,
    pub size: u64,
    pub offset: u64,
}

/// The file offset of a loadable address: a dynamic entry stores a load address, and the segment
/// that maps it decides which file bytes those are.
pub fn load_address_offset(loads: &[LoadSegment], address: u64) -> Option<u64> {
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
}

impl LibraryIdentity {
    pub fn new(path: PathBuf, version: String, arch: Arch, family: Family, source: Origin) -> Self {
        Self {
            path,
            version,
            arch,
            family,
            source,
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

    /// The one line the doctor prints for the library it loaded.
    pub fn report(&self) -> String {
        identity_report(
            &self.path.display().to_string(),
            &self.version,
            self.arch.label(),
            self.family.label(),
            self.source.label(),
        )
    }
}

/// The single doctor line an identity renders; both the loader identity and the doctor-facing one
/// use it, so the five fields always appear in the same order.
pub fn identity_report(
    path: &str,
    version: &str,
    architecture: &str,
    family: &str,
    source: &str,
) -> String {
    format!("{path} version {version} architecture {architecture} family {family} source {source}")
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

/// Linux: the SAP GUI for Java package first, then the SDK family; ICU stays in `unicode/`.
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
            [
                root.join("bin").join(gui_library_filename(Platform::Linux)),
                root.join("bin")
                    .join("unicode")
                    .join(rfc_library_filename(Platform::Linux)),
            ]
        })
        .filter(|path| fs.is_file(path))
        .collect();

    GuiDiscovery {
        candidates,
        notes: Vec::new(),
    }
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

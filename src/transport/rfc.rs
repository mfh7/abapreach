use super::{Request, Response, SadtTransport, TransportError};
use crate::config::Connection;
use libloading::{Library, Symbol};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fs;
use std::mem::ManuallyDrop;
use std::os::raw::{c_int, c_uint};
use std::path::{Path, PathBuf};

pub const SADT_REST_RFC_ENDPOINT: &str = "SADT_REST_RFC_ENDPOINT";

/// The library reads this when it is mapped, before any symbol can be called.
pub const RFC_TRACE_DIR_ENV: &str = "RFC_TRACE_DIR";

const RFC_LIBRARY_OVERRIDE: &str = "ABAPREACH_RFC_LIBRARY";
const MAX_GUI_SEARCH_DEPTH: usize = 4;
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
}

impl Env {
    pub fn current() -> Self {
        Self {
            override_path: std::env::var_os(RFC_LIBRARY_OVERRIDE).map(PathBuf::from),
            gui_paths: macos_gui_paths(),
            sdk_paths: sdk_paths(),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn test_paths(override_path: &str, gui_path: &str, sdk_path: &str) -> Self {
        Self {
            override_path: Some(PathBuf::from(override_path)),
            gui_paths: vec![PathBuf::from(gui_path)],
            sdk_paths: vec![PathBuf::from(sdk_path)],
        }
    }
}

pub struct RfcLibrary {
    path: PathBuf,
    // Never unloaded: SAP's library leaves thread-local destructors that crash after dlclose.
    library: ManuallyDrop<Library>,
}

impl RfcLibrary {
    pub fn candidate_paths(env: &Env) -> Vec<PathBuf> {
        env.override_path
            .iter()
            .chain(env.gui_paths.iter())
            .chain(env.sdk_paths.iter())
            .cloned()
            .collect()
    }

    pub fn load(env: &Env) -> Result<Self, String> {
        let candidates = Self::candidate_paths(env);

        if candidates.is_empty() {
            return Err("RFC library not found; set ABAPREACH_RFC_LIBRARY".to_owned());
        }

        point_trace_at_temp_dir();
        let mut errors = Vec::new();

        for path in candidates {
            match unsafe { Library::new(&path) } {
                Ok(library) => {
                    set_trace_directory(&library);

                    return Ok(Self {
                        path,
                        library: ManuallyDrop::new(library),
                    });
                }
                Err(error) => errors.push(format!("{}: {error}", path.display())),
            }
        }

        Err(format!("could not load RFC library: {}", errors.join("; ")))
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

    pub fn library(&self) -> &Library {
        &self.library
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

fn macos_gui_paths() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }

    let mut paths = Vec::new();

    collect_gui_paths(Path::new("/Applications"), MAX_GUI_SEARCH_DEPTH, &mut paths);
    paths.sort();
    paths.dedup();
    paths
}

fn collect_gui_paths(root: &Path, depth: usize, paths: &mut Vec<PathBuf>) {
    let candidate = root
        .join("Contents")
        .join("Resources")
        .join("bin")
        .join("unicode")
        .join("libsapnwrfc.dylib");

    if candidate.is_file() {
        paths.push(candidate);
    }

    if depth == 0 {
        return;
    }

    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            collect_gui_paths(&path, depth - 1, paths);
        }
    }
}

fn sdk_paths() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(home) = std::env::var_os("SAPNWRFC_HOME") {
        roots.push(PathBuf::from(home));
    }

    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("sap").join("nwrfcsdk"));
    }

    roots.push(PathBuf::from("/opt/sap/nwrfcsdk"));
    roots.push(PathBuf::from("/usr/local/sap/nwrfcsdk"));

    roots
        .into_iter()
        .map(|root| root.join("lib").join(rfc_library_filename()))
        .collect()
}

fn rfc_library_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "sapnwrfc.dll"
    } else if cfg!(target_os = "macos") {
        "libsapnwrfc.dylib"
    } else {
        "libsapnwrfc.so"
    }
}

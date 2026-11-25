//! The generic function-module call in both character sizes, against a fake RFC
//! library built per test: UTF-16 like the NW RFC SDK, and single-byte like the SAP GUI 8.00 for
//! Windows library. The fake keeps real handles (function, structure, table, row), so a call on
//! the wrong kind of handle fails as it does in the SDK.

use abapreach::transport::TransportError;
use abapreach::transport::rfc::{
    Cell, CellKind, ColumnRead, Direction, DynamicRfcApi, Env, FunctionApi, FunctionCall,
    ParameterType, RfcApi, RfcEndpointRequest, RfcLibrary, RfcSymbols, TableRead, TableWrite,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FAKE_BASE: &str = r#"
use std::collections::HashMap;
use std::os::raw::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicU32, Ordering};

static CREATES: AtomicU32 = AtomicU32::new(0);
static DESTROYS: AtomicU32 = AtomicU32::new(0);
static SETS: AtomicU32 = AtomicU32::new(0);
static FAIL_CREATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static BODY: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
// Which stage of RFC_SYSTEM_INFO fails: 1 invoke, 2 get structure, 3 get string of RFCSAPRL,
// 4 get string of RFCHOST, 5 get string length of RFCKERNRL. 0 none.
static SYSINFO_FAIL: AtomicU32 = AtomicU32::new(0);
static STRING_GETS: AtomicU32 = AtomicU32::new(0);
static STRING_LENGTHS: AtomicU32 = AtomicU32::new(0);
static STRUCTURE_GETS: AtomicU32 = AtomicU32::new(0);

#[no_mangle] pub extern "C" fn fake_creates() -> u32 { CREATES.load(Ordering::SeqCst) }
#[no_mangle] pub extern "C" fn fake_destroys() -> u32 { DESTROYS.load(Ordering::SeqCst) }
#[no_mangle] pub extern "C" fn fake_sets() -> u32 { SETS.load(Ordering::SeqCst) }
#[no_mangle] pub extern "C" fn fake_fail_next_create() -> u32 { FAIL_CREATE.store(true, Ordering::SeqCst); 0 }
#[no_mangle] pub extern "C" fn fake_sysinfo_fail(stage: u32) -> u32 { SYSINFO_FAIL.store(stage, Ordering::SeqCst); 0 }
#[no_mangle] pub extern "C" fn fake_string_gets() -> u32 { STRING_GETS.load(Ordering::SeqCst) }
#[no_mangle] pub extern "C" fn fake_string_lengths() -> u32 { STRING_LENGTHS.load(Ordering::SeqCst) }
#[no_mangle] pub extern "C" fn fake_structure_gets() -> u32 { STRUCTURE_GETS.load(Ordering::SeqCst) }

const FN_YFM: usize = 0x10;
const FN_SADT: usize = 0x20;
const FN_NULL_CREATE: usize = 0x30;
const FN_ABAP_EXC: usize = 0x40;
const FN_HUGE: usize = 0x50;
const FN_NULL_ROW: usize = 0x60;
const FN_READ_TABLE: usize = 0x70;
const FN_SYSINFO: usize = 0x80;

unsafe fn read(pointer: *const c_void) -> String {
    if WIDE {
        let units = pointer as *const u16;
        let mut text = Vec::new();
        while *units.add(text.len()) != 0 {
            text.push(*units.add(text.len()));
        }
        String::from_utf16_lossy(&text)
    } else {
        std::ffi::CStr::from_ptr(pointer as *const _).to_string_lossy().into_owned()
    }
}

unsafe fn chars_of(pointer: *const c_void, length: usize) -> String {
    if WIDE {
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer as *const u16, length))
    } else {
        String::from_utf8_lossy(std::slice::from_raw_parts(pointer as *const u8, length)).into_owned()
    }
}

unsafe fn write_at(buffer: *mut c_void, text: &str, terminate: bool) {
    if WIDE {
        let units = buffer as *mut u16;
        let mut count = 0;
        for unit in text.encode_utf16() {
            *units.add(count) = unit;
            count += 1;
        }
        if terminate { *units.add(count) = 0; }
    } else {
        let bytes = buffer as *mut u8;
        for (index, byte) in text.bytes().enumerate() {
            *bytes.add(index) = byte;
        }
        if terminate { *bytes.add(text.len()) = 0; }
    }
}

unsafe fn write(buffer: *mut c_void, text: &str) { write_at(buffer, text, true) }

unsafe fn fail_key(info: *mut c_void, code: c_int, key: &str, message: &str) -> c_int {
    *(info as *mut c_int) = code;
    write((info as *mut u8).add(8) as *mut c_void, key);
    let offset = if WIDE { 264 } else { 136 };
    write((info as *mut u8).add(offset) as *mut c_void, message);
    code
}

unsafe fn fail(info: *mut c_void, code: c_int, message: &str) -> c_int {
    fail_key(info, code, "", message)
}

enum Value { Text(String), Int(i32) }

struct Node {
    kind: u8, // 0 function, 1 structure, 2 table, 3 row
    function: usize,
    fields: HashMap<String, Value>,
    children: HashMap<String, *mut Node>,
    rows: Vec<*mut Node>,
    cursor: usize,
    huge: bool,
    null_row: bool,
    fail_read: bool,
}

fn node(kind: u8, function: usize) -> *mut c_void {
    Box::into_raw(Box::new(Node {
        kind, function, fields: HashMap::new(), children: HashMap::new(), rows: Vec::new(),
        cursor: 0, huge: false, null_row: false, fail_read: false,
    })) as *mut c_void
}

unsafe fn at<'a>(handle: *mut c_void) -> &'a mut Node { &mut *(handle as *mut Node) }

fn strict(function: usize) -> bool { function != FN_SADT }

#[no_mangle]
pub unsafe extern "C" fn RfcGetVersion(a: *mut c_uint, b: *mut c_uint, c: *mut c_uint) -> *const c_void {
    *a = 7;
    *b = 53;
    *c = 1524;
    if WIDE {
        static TEXT: [u16; 4] = [55, 53, 51, 0];
        TEXT.as_ptr() as *const c_void
    } else {
        b"753\0".as_ptr() as *const c_void
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcSetTraceDir(_: *const c_void, _: *mut c_void) -> c_int { 0 }

#[no_mangle]
pub unsafe extern "C" fn RfcOpenConnection(_: *const c_void, _: c_uint, _: *mut c_void) -> *mut c_void {
    1 as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn RfcPing(_: *mut c_void, _: *mut c_void) -> c_int { 0 }

#[no_mangle]
pub unsafe extern "C" fn RfcGetFunctionDesc(_: *mut c_void, name: *const c_void, info: *mut c_void) -> *mut c_void {
    match read(name).as_str() {
        "YTEST_FAKE_FUNCTION" => FN_YFM as *mut c_void,
        "SADT_REST_RFC_ENDPOINT" => FN_SADT as *mut c_void,
        "NULL_CREATE" => FN_NULL_CREATE as *mut c_void,
        "ABAP_EXC" => FN_ABAP_EXC as *mut c_void,
        "HUGE_TABLE" => FN_HUGE as *mut c_void,
        "NULL_ROW" => FN_NULL_ROW as *mut c_void,
        "RFC_READ_TABLE" => FN_READ_TABLE as *mut c_void,
        "RFC_SYSTEM_INFO" => FN_SYSINFO as *mut c_void,
        "MISSING_FM" => { fail_key(info, 5, "FU_NOT_FOUND", "function module not found"); std::ptr::null_mut() }
        _ => { fail(info, 20, "bad function"); std::ptr::null_mut() }
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcCreateFunction(descriptor: *mut c_void, info: *mut c_void) -> *mut c_void {
    CREATES.fetch_add(1, Ordering::SeqCst);
    if descriptor as usize == FN_NULL_CREATE || FAIL_CREATE.swap(false, Ordering::SeqCst) {
        fail(info, 3, "cannot create");
        return std::ptr::null_mut();
    }
    node(0, descriptor as usize)
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetStructure(container: *mut c_void, name: *const c_void, out: *mut *mut c_void, info: *mut c_void) -> c_int {
    let parent = at(container);
    let name = read(name);
    if parent.function == FN_SYSINFO {
        // Only the exporting structure of the function itself, and it exists once invoked.
        STRUCTURE_GETS.fetch_add(1, Ordering::SeqCst);
        if parent.kind != 0 || name != "RFCSI_EXPORT" || SYSINFO_FAIL.load(Ordering::SeqCst) == 2 {
            return fail(info, 20, "bad structure");
        }
        match parent.children.get("RFCSI_EXPORT") {
            Some(child) => { *out = *child as *mut c_void; return 0; }
            None => return fail(info, 20, "structure read before the invoke"),
        }
    }
    if parent.kind > 1 || strict(parent.function) {
        return fail(info, 20, "bad structure");
    }
    let function = parent.function;
    let child = *parent.children.entry(name).or_insert_with(|| node(1, function) as *mut Node);
    *out = child as *mut c_void;
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetTable(container: *mut c_void, name: *const c_void, out: *mut *mut c_void, info: *mut c_void) -> c_int {
    let parent = at(container);
    let name = read(name);
    let allowed = if parent.function == FN_READ_TABLE {
        parent.kind == 0 && matches!(name.as_str(), "FIELDS" | "OPTIONS" | "DATA")
    } else if strict(parent.function) {
        parent.kind == 0 && matches!(name.as_str(), "IT_SOURCE" | "ET_SOURCE" | "ET_MESSAGES")
    } else {
        parent.kind == 1 && name == "HEADER_FIELDS"
    };
    if !allowed {
        return fail(info, 20, "bad table");
    }
    let function = parent.function;
    let child = *parent.children.entry(name).or_insert_with(|| node(2, function) as *mut Node);
    *out = child as *mut c_void;
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcAppendNewRow(table: *mut c_void, info: *mut c_void) -> *mut c_void {
    let table = at(table);
    if table.kind != 2 {
        fail(info, 20, "not a table");
        return std::ptr::null_mut();
    }
    let row = node(3, table.function) as *mut Node;
    table.rows.push(row);
    row as *mut c_void
}

#[no_mangle]
pub unsafe extern "C" fn RfcSetString(_: *mut c_void, name: *const c_void, value: *const c_void, length: c_uint, info: *mut c_void) -> c_int {
    let (name, value) = (read(name), read(value));
    if length as usize != value.encode_utf16().count() { return fail(info, 20, "bad length"); }
    match (name.as_str(), value.as_str()) {
        ("METHOD", "GET") | ("URI", "/sap/bc/adt/discovery") | ("VERSION", "HTTP/1.1") | ("NAME", "Accept") | ("VALUE", "text/plain") => 0,
        _ => fail(info, 20, "bad field"),
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcSetXString(_: *mut c_void, _: *const c_void, value: *const c_void, length: c_uint, _: *mut c_void) -> c_int {
    *BODY.lock().unwrap() = std::slice::from_raw_parts(value as *const u8, length as usize).to_vec();
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetStringLength(container: *mut c_void, name: *const c_void, length: *mut c_uint, info: *mut c_void) -> c_int {
    if at(container).function == FN_SYSINFO {
        STRING_LENGTHS.fetch_add(1, Ordering::SeqCst);
        let name = read(name);
        if at(container).kind != 1 || (name == "RFCKERNRL" && SYSINFO_FAIL.load(Ordering::SeqCst) == 5) {
            return fail(info, 20, "bad length container");
        }
        return match at(container).fields.get(&name) {
            Some(Value::Text(text)) => { *length = text.chars().count() as c_uint; 0 }
            _ => fail(info, 20, "bad length name"),
        };
    }
    match read(name).as_str() {
        "STATUS_CODE" => { *length = 3; 0 }
        "MESSAGE_BODY" => { *length = BODY.lock().unwrap().len().max(2) as c_uint; 0 }
        _ => fail(info, 20, "bad length name"),
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetString(container: *mut c_void, name: *const c_void, buffer: *mut c_void, size: c_uint, written: *mut c_uint, info: *mut c_void) -> c_int {
    if at(container).function == FN_SYSINFO {
        STRING_GETS.fetch_add(1, Ordering::SeqCst);
        let name = read(name);
        let stage = SYSINFO_FAIL.load(Ordering::SeqCst);
        if at(container).kind != 1 || (name == "RFCSAPRL" && stage == 3) || (name == "RFCHOST" && stage == 4) {
            return fail(info, 20, "bad get string");
        }
        let text = match at(container).fields.get(&name) {
            Some(Value::Text(text)) => text.clone(),
            _ => return fail(info, 20, "bad get string name"),
        };
        // The buffer is the length the library reported for this field, plus the terminator.
        if size as usize != text.chars().count() + 1 { return fail(info, 13, "bad buffer size"); }
        write(buffer, &text);
        *written = text.chars().count() as c_uint;
        return 0;
    }
    if read(name) != "STATUS_CODE" || size < 4 { return fail(info, 20, "bad get string"); }
    write(buffer, "200");
    *written = 3;
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcGetXString(_: *mut c_void, _: *const c_void, buffer: *mut c_void, size: c_uint, written: *mut c_uint, info: *mut c_void) -> c_int {
    let kept = BODY.lock().unwrap().clone();
    let bytes = if kept.is_empty() { b"ok".to_vec() } else { kept };
    if (size as usize) < bytes.len() { return fail(info, 20, "bad get xstring size"); }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len());
    *written = bytes.len() as c_uint;
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcInvoke(_: *mut c_void, function: *mut c_void, info: *mut c_void) -> c_int {
    let function = at(function);
    match function.function {
        FN_ABAP_EXC => fail_key(info, 5, "ZCX_EXC_KEY", "fake abap exception"),
        FN_SYSINFO => {
            if SYSINFO_FAIL.load(Ordering::SeqCst) == 1 {
                return fail_key(info, 5, "SYSINFO_INVOKE", "system info invoke failed");
            }
            // Fields are blank-padded to the width of RFCSI_EXPORT, as the function module fills them.
            let export = node(1, FN_SYSINFO) as *mut Node;
            for (name, value, width) in [
                ("RFCDEST", "apphost_HD1_00", 32usize),
                ("RFCHOST", "apphost", 8),
                ("RFCSYSID", "HD1", 8),
                ("RFCSAPRL", "750", 4),
                ("RFCKERNRL", "753", 4),
                ("RFCDBSYS", "DB6", 10),
            ] {
                at(export as *mut c_void).fields.insert(name.into(), Value::Text(format!("{value:<width$}")));
            }
            function.children.insert("RFCSI_EXPORT".into(), export);
            0
        }
        FN_YFM => {
            let text = |node: &Node, name: &str| match node.fields.get(name) {
                Some(Value::Text(text)) => text.clone(),
                _ => String::new(),
            };
            let mode = text(function, "IV_MODE");
            let name = text(function, "IV_OBJECT_NAME");
            function.fields.insert("EV_STATUS".into(), Value::Text(format!("{mode}_OK")));
            function.fields.insert("EV_LIVE_HASH".into(), Value::Text(format!("hash-{name}")));

            let source = function.children.get("IT_SOURCE").map(|table| at(*table as *mut c_void).rows.clone()).unwrap_or_default();
            let echo = node(2, FN_YFM) as *mut Node;
            for line in source {
                let row = node(3, FN_YFM) as *mut Node;
                at(row as *mut c_void).fields.insert("LINE".into(), Value::Text(text(at(line as *mut c_void), "LINE")));
                at(echo as *mut c_void).rows.push(row);
            }
            function.children.insert("ET_SOURCE".into(), echo);

            let messages = node(2, FN_YFM) as *mut Node;
            let list: Vec<(&str, i32, String)> = if name == "MULTI" {
                vec![("S", 0, "first message".into()), ("W", 1, "second message".into()), ("E", -2, format!("third message for {mode}"))]
            } else {
                vec![("I", 0, format!("{mode} {name}"))]
            };
            for (kind, number, message) in list {
                let row = node(3, FN_YFM) as *mut Node;
                at(row as *mut c_void).fields.insert("TYPE".into(), Value::Text(kind.into()));
                at(row as *mut c_void).fields.insert("ROW".into(), Value::Int(number));
                at(row as *mut c_void).fields.insert("MESSAGE".into(), Value::Text(message));
                at(messages as *mut c_void).rows.push(row);
            }
            function.children.insert("ET_MESSAGES".into(), messages);
            0
        }
        FN_READ_TABLE => {
            let text = |node: &Node, name: &str| match node.fields.get(name) {
                Some(Value::Text(text)) => text.clone(),
                _ => String::new(),
            };
            let table = text(function, "QUERY_TABLE");
            if table == "ZFAIL_INVOKE" {
                return fail_key(info, 5, "TABLE_NOT_AVAILABLE", "table is not available");
            }
            let wanted: usize = text(function, "ROWCOUNT").trim().parse().unwrap_or(0);
            let fields = at(*function.children.get("FIELDS").expect("FIELDS") as *mut c_void);
            let width = |name: &str| match name {
                "SEQNO" | "MANDT" | "MODNO" => 3,
                "DATUM" => 8,
                "UZEIT" => 6,
                "AHOST" => 32,
                "UNAME" => 12,
                "TLINE" => 255,
                name if name.starts_with("FLIST") => 200,
                _ => 10,
            };
            let value = |name: &str, index: usize| match name {
                "SEQNO" => format!("{index:03}"),
                "MANDT" => "110".to_string(),
                "DATUM" => "20261103".to_string(),
                "UZEIT" => "053726".to_string(),
                "AHOST" => "apphost".to_string(),
                "UNAME" => "TESTUSER".to_string(),
                "MODNO" => "21".to_string(),
                "FLIST" => "FC007X_ERROR".to_string(),
                name if name.starts_with("FLIST") => "x".repeat(200),
                _ => "v".to_string(),
            };
            let mut names = Vec::new();
            let mut offset = 0;
            for row in fields.rows.clone() {
                let row = at(row as *mut c_void);
                let name = text(row, "FIELDNAME");
                row.fields.insert("OFFSET".into(), Value::Text(format!("{offset:06}")));
                row.fields.insert("LENGTH".into(), Value::Text(format!("{:06}", width(&name))));
                offset += width(&name);
                names.push(name);
            }
            let data = node(2, FN_READ_TABLE) as *mut Node;
            for index in 0..wanted.min(3) {
                let row = node(3, FN_READ_TABLE) as *mut Node;
                let line: String = names.iter().map(|name| format!("{:<w$}", value(name, index), w = width(name))).collect();
                at(row as *mut c_void).fields.insert("WA".into(), Value::Text(line));
                at(row as *mut c_void).fail_read = table == "ZFAIL_READ";
                at(data as *mut c_void).rows.push(row);
            }
            function.children.insert("DATA".into(), data);
            0
        }
        FN_HUGE => {
            let table = node(2, FN_HUGE) as *mut Node;
            at(table as *mut c_void).huge = true;
            function.children.insert("ET_MESSAGES".into(), table);
            0
        }
        FN_NULL_ROW => {
            let table = node(2, FN_NULL_ROW) as *mut Node;
            at(table as *mut c_void).null_row = true;
            at(table as *mut c_void).rows.push(node(3, FN_NULL_ROW) as *mut Node);
            function.children.insert("ET_MESSAGES".into(), table);
            0
        }
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn RfcDestroyFunction(_: *mut c_void, _: *mut c_void) -> c_int {
    DESTROYS.fetch_add(1, Ordering::SeqCst);
    0
}

#[no_mangle]
pub unsafe extern "C" fn RfcCloseConnection(_: *mut c_void, _: *mut c_void) -> c_int { 0 }
"#;

/// The optional function-module symbols, one source each so a test can leave out exactly one.
const FAKE_EXT: [(&str, &str); 8] = [
    (
        "RfcGetRowCount",
        r#"
#[no_mangle]
pub unsafe extern "C" fn RfcGetRowCount(table: *mut c_void, count: *mut c_uint, info: *mut c_void) -> c_int {
    let table = at(table);
    if table.kind != 2 { return fail(info, 20, "not a table"); }
    *count = if table.huge { 1_000_000 } else { table.rows.len() as c_uint };
    0
}
"#,
    ),
    (
        "RfcMoveTo",
        r#"
#[no_mangle]
pub unsafe extern "C" fn RfcMoveTo(table: *mut c_void, index: c_uint, info: *mut c_void) -> c_int {
    let table = at(table);
    if table.kind != 2 || index as usize >= table.rows.len() { return fail(info, 20, "bad index"); }
    table.cursor = index as usize;
    0
}
"#,
    ),
    (
        "RfcGetCurrentRow",
        r#"
#[no_mangle]
pub unsafe extern "C" fn RfcGetCurrentRow(table: *mut c_void, info: *mut c_void) -> *mut c_void {
    let table = at(table);
    if table.kind != 2 || table.null_row {
        fail(info, 20, "no current row");
        return std::ptr::null_mut();
    }
    table.rows[table.cursor] as *mut c_void
}
"#,
    ),
    (
        "RfcGetInt",
        r#"
#[no_mangle]
pub unsafe extern "C" fn RfcGetInt(container: *mut c_void, name: *const c_void, out: *mut c_int, info: *mut c_void) -> c_int {
    match at(container).fields.get(&read(name)) {
        Some(Value::Int(value)) => { *out = *value; 0 }
        _ => fail(info, 22, "conversion failure"),
    }
}
"#,
    ),
    (
        "RfcGetChars",
        r#"
#[no_mangle]
pub unsafe extern "C" fn RfcGetChars(container: *mut c_void, name: *const c_void, buffer: *mut c_void, length: c_uint, info: *mut c_void) -> c_int {
    let name = read(name);
    if name == "WA" && length != 512 { return fail(info, 13, "the data row is 512 characters wide"); }
    if name == "WA" && at(container).fail_read { return fail(info, 22, "conversion failure"); }
    let text = match at(container).fields.get(&name) {
        Some(Value::Text(text)) => text.clone(),
        Some(Value::Int(_)) => return fail(info, 22, "conversion failure"),
        None => String::new(),
    };
    if text.chars().count() > length as usize { return fail(info, 13, "buffer too small"); }
    // Padded with blanks to the requested length and never terminated, as the SDK does.
    let padded = format!("{text:<width$}", width = length as usize);
    write_at(buffer, &padded, false);
    0
}
"#,
    ),
    (
        "RfcSetChars",
        r#"
#[no_mangle]
pub unsafe extern "C" fn RfcSetChars(container: *mut c_void, name: *const c_void, value: *const c_void, length: c_uint, info: *mut c_void) -> c_int {
    SETS.fetch_add(1, Ordering::SeqCst);
    let target = at(container);
    let name = read(name);
    // The length counts characters, not bytes.
    let text = chars_of(value, length as usize);
    let allowed = match target.kind {
        0 => matches!(name.as_str(), "IV_OBJECT_TYPE" | "IV_OBJECT_NAME" | "IV_MODE" | "IV_PACKAGE" | "IV_TEXT")
            || (target.function == FN_READ_TABLE && matches!(name.as_str(), "QUERY_TABLE" | "ROWCOUNT")),
        3 => name == "LINE"
            || (target.function == FN_READ_TABLE && matches!(name.as_str(), "FIELDNAME" | "TEXT")),
        _ => false,
    };
    if !allowed { return fail(info, 20, "wrong type"); }
    if name == "QUERY_TABLE" && text == "ZFAIL_SET" { return fail(info, 20, "cannot set the table"); }
    if name == "TEXT" && length > 72 { return fail(info, 13, "buffer too small"); }
    if name == "FIELDNAME" && length > 30 { return fail(info, 13, "buffer too small"); }
    if name == "IV_MODE" && length > 10 { return fail(info, 13, "buffer too small"); }
    if name == "LINE" && length > 255 { return fail(info, 13, "buffer too small"); }
    target.fields.insert(name, Value::Text(text));
    0
}
"#,
    ),
    (
        "RfcGetParameterCount",
        r#"
#[no_mangle]
pub unsafe extern "C" fn RfcGetParameterCount(descriptor: *mut c_void, count: *mut c_uint, info: *mut c_void) -> c_int {
    if descriptor as usize != FN_YFM { return fail(info, 20, "bad descriptor"); }
    *count = 4;
    0
}
"#,
    ),
    (
        "RfcGetParameterDescByIndex",
        r#"
#[no_mangle]
pub unsafe extern "C" fn RfcGetParameterDescByIndex(descriptor: *mut c_void, index: c_uint, out: *mut c_void, info: *mut c_void) -> c_int {
    if descriptor as usize != FN_YFM || index > 3 { return fail(info, 20, "bad parameter"); }
    // (name, RFCTYPE, RFC_DIRECTION, nucLength, ucLength, optional)
    let (name, kind, direction, nuc, uc, optional) = [
        ("IV_MODE", 0u32, 1u32, 10u32, 20u32, 0u8),
        ("IT_SOURCE", 5, 1, 255, 510, 1),
        ("ET_MESSAGES", 5, 2, 548, 1088, 0),
        ("EV_LIVE_HASH", 0, 2, 64, 128, 0),
    ][index as usize];
    // Byte offsets written down independently of the product: 31 characters of name, then
    // type, direction, nucLength and ucLength, and `optional` after the 80-character text.
    let (kind_at, direction_at, nuc_at, uc_at, optional_at) =
        if WIDE { (64, 68, 72, 76, 318) } else { (32, 36, 40, 44, 175) };
    let base = out as *mut u8;
    write(out, name);
    *(base.add(kind_at) as *mut u32) = kind;
    *(base.add(direction_at) as *mut u32) = direction;
    *(base.add(nuc_at) as *mut u32) = nuc;
    *(base.add(uc_at) as *mut u32) = uc;
    *base.add(optional_at) = optional;
    0
}
"#,
    ),
];

fn build(directory: &Path, wide: bool, omit: &[&str]) -> PathBuf {
    build_without_base(directory, wide, omit, &[])
}

/// Like [`build`], and the library does not export the base symbols `rename`: the function stays
/// in the source under another name.
fn build_without_base(directory: &Path, wide: bool, omit: &[&str], rename: &[&str]) -> PathBuf {
    let source = directory.join("fake.rs");
    let library = directory.join(format!(
        "fake_ext_{}.{}",
        if wide { "wide" } else { "narrow" },
        std::env::consts::DLL_EXTENSION
    ));
    let mut base = FAKE_BASE.to_owned();

    for name in rename {
        base = base.replace(&format!("fn {name}("), &format!("fn renamed_{name}("));
    }

    let mut text = format!("#![allow(dead_code)]\nconst WIDE: bool = {wide};\n{base}");

    for (name, code) in FAKE_EXT {
        if !omit.contains(&name) {
            text.push_str(code);
        }
    }

    std::fs::write(&source, text).unwrap();

    let status = std::process::Command::new(std::env::var_os("RUSTC").unwrap_or("rustc".into()))
        .args(["--crate-type", "cdylib", "--crate-name", "fake_ext"])
        .arg("-o")
        .arg(&library)
        .arg(&source)
        .status()
        .expect("rustc must be on the PATH to build the fake RFC library");

    assert!(status.success(), "rustc could not build the fake library");
    library
}

struct Fixture {
    // Keeps the library file for the lifetime of the test.
    _directory: tempfile::TempDir,
    path: PathBuf,
    api: DynamicRfcApi,
}

impl Fixture {
    fn new(wide: bool, omit: &[&str]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = build(directory.path(), wide, omit);
        let library = RfcLibrary::load(&Env::test_paths(path.to_str().unwrap(), "", ""))
            .unwrap_or_else(|error| panic!("the fake must load: {error}"));
        let parameters = [
            ("ASHOST", "good.example.com"),
            ("SYSNR", "00"),
            ("CLIENT", "100"),
            ("USER", "user"),
            ("PASSWD", "secret"),
            ("LANG", "EN"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .to_vec();

        Self {
            _directory: directory,
            path,
            api: DynamicRfcApi::load(library, parameters).unwrap(),
        }
    }

    /// A counter the fake keeps; the same path maps the same library instance.
    fn counter(&self, name: &[u8]) -> u32 {
        counter_at(&self.path, name)
    }
}

fn counter_at(path: &Path, name: &[u8]) -> u32 {
    let library = unsafe { libloading::Library::new(path) }.unwrap();
    let counter: libloading::Symbol<unsafe extern "C" fn() -> u32> =
        unsafe { library.get(name) }.unwrap();

    unsafe { counter() }
}

const WIDTHS: [bool; 2] = [true, false];

fn column(name: &str, kind: CellKind) -> ColumnRead {
    ColumnRead {
        name: name.to_owned(),
        kind,
    }
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn deploy_call(name: &str, mode: &str, lines: &[&str]) -> FunctionCall {
    FunctionCall {
        function: "YTEST_FAKE_FUNCTION".to_owned(),
        imports: pairs(&[
            ("IV_OBJECT_TYPE", "TRAN"),
            ("IV_OBJECT_NAME", name),
            ("IV_MODE", mode),
        ]),
        tables: vec![TableWrite {
            name: "IT_SOURCE".to_owned(),
            rows: lines.iter().map(|line| pairs(&[("LINE", line)])).collect(),
        }],
        exports: vec![
            column("EV_STATUS", CellKind::Chars(20)),
            column("EV_LIVE_HASH", CellKind::Chars(64)),
        ],
        export_tables: vec![
            TableRead {
                name: "ET_SOURCE".to_owned(),
                columns: vec![column("LINE", CellKind::Chars(255))],
            },
            TableRead {
                name: "ET_MESSAGES".to_owned(),
                columns: vec![
                    column("TYPE", CellKind::Chars(1)),
                    column("ROW", CellKind::Int),
                    column("MESSAGE", CellKind::Chars(220)),
                ],
            },
        ],
    }
}

fn text(value: &str) -> Cell {
    Cell::Text(value.to_owned())
}

fn call_named(function: &str) -> FunctionCall {
    FunctionCall {
        function: function.to_owned(),
        ..FunctionCall::default()
    }
}

#[test]
fn the_ext_symbols_are_the_optional_part_of_the_abi() {
    assert_eq!(
        RfcSymbols::optional_names(),
        vec![
            "RfcGetRowCount",
            "RfcMoveTo",
            "RfcGetCurrentRow",
            "RfcGetInt",
            "RfcGetChars",
            "RfcSetChars",
            "RfcGetParameterCount",
            "RfcGetParameterDescByIndex",
        ]
    );

    let required = RfcSymbols::required_names();

    for name in RfcSymbols::optional_names() {
        assert!(!required.contains(&name), "{name} must stay optional");
    }
}

#[test]
fn fields_and_tables_round_trip_in_either_width() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);
        let result = fixture
            .api
            .call_function(&deploy_call(
                "YTEST1",
                "READ",
                &["REPORT YTEST1.", "WRITE 'a'."],
            ))
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        assert_eq!(
            result.exports,
            vec![
                ("EV_STATUS".to_owned(), text("READ_OK")),
                ("EV_LIVE_HASH".to_owned(), text("hash-YTEST1")),
            ],
            "wide: {wide}"
        );
        assert_eq!(result.tables[0].0, "ET_SOURCE");
        assert_eq!(
            result.tables[0].1,
            vec![vec![text("REPORT YTEST1.")], vec![text("WRITE 'a'.")]],
            "wide: {wide}"
        );
        assert_eq!(fixture.counter(b"fake_creates\0"), 1);
        assert_eq!(fixture.counter(b"fake_destroys\0"), 1);
        // Three header fields and two source lines.
        assert_eq!(fixture.counter(b"fake_sets\0"), 5);
    }
}

#[test]
fn a_multi_row_message_table_reads_text_and_int_cells_in_either_width() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);
        let result = fixture
            .api
            .call_function(&deploy_call("MULTI", "CREATE", &[]))
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));
        let (name, rows) = &result.tables[1];

        assert_eq!(name, "ET_MESSAGES");
        assert_eq!(
            rows,
            &vec![
                vec![text("S"), Cell::Int(0), text("first message")],
                vec![text("W"), Cell::Int(1), text("second message")],
                // A negative INT4 and a message that is shorter than its field (blanks trimmed).
                vec![text("E"), Cell::Int(-2), text("third message for CREATE")],
            ],
            "wide: {wide}"
        );
        assert!(result.tables[0].1.is_empty(), "no source lines were sent");
    }
}

#[test]
fn a_non_ascii_line_reaches_a_utf16_library_and_is_refused_by_a_single_byte_one() {
    let mut wide = Fixture::new(true, &[]);
    let result = wide
        .api
        .call_function(&deploy_call("YTEST1", "READ", &["* Ärger"]))
        .unwrap();

    assert_eq!(result.tables[0].1, vec![vec![text("* Ärger")]]);

    let mut narrow = Fixture::new(false, &[]);
    let error = narrow
        .api
        .call_function(&deploy_call("YTEST1", "READ", &["* Ärger"]))
        .unwrap_err();

    assert!(
        matches!(&error, TransportError::Protocol(message)
            if message.contains("a single-byte RFC library accepts ASCII only")),
        "{error:?}"
    );
    // The refusal came before the invoke, so nothing was sent; the function is still destroyed.
    assert!(!error.was_sent());
    assert_eq!(
        narrow.counter(b"fake_destroys\0"),
        narrow.counter(b"fake_creates\0")
    );
}

#[test]
fn an_abap_exception_carries_its_key_and_text_and_counts_as_sent() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);
        let error = fixture
            .api
            .call_function(&call_named("ABAP_EXC"))
            .unwrap_err();

        assert_eq!(
            error,
            TransportError::Sent(Box::new(TransportError::Protocol(
                "RfcInvoke [ZCX_EXC_KEY]: fake abap exception (RFC code 5)".to_owned()
            ))),
            "wide: {wide}"
        );
        assert!(error.was_sent());
        assert_eq!(
            fixture.counter(b"fake_destroys\0"),
            fixture.counter(b"fake_creates\0"),
            "the function is destroyed after a failed invoke"
        );
    }
}

#[test]
fn a_function_description_lists_its_parameters_in_either_width() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);
        let info = fixture
            .api
            .describe_function("YTEST_FAKE_FUNCTION")
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"))
            .expect("the function exists");
        let summary = info
            .parameters
            .iter()
            .map(|parameter| {
                (
                    parameter.name.as_str(),
                    parameter.kind,
                    parameter.direction,
                    parameter.nuc_length,
                    parameter.uc_length,
                    parameter.optional,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(info.name, "YTEST_FAKE_FUNCTION");
        assert_eq!(
            summary,
            vec![
                (
                    "IV_MODE",
                    ParameterType::Char,
                    Direction::Import,
                    10,
                    20,
                    false
                ),
                (
                    "IT_SOURCE",
                    ParameterType::Table,
                    Direction::Import,
                    255,
                    510,
                    true
                ),
                (
                    "ET_MESSAGES",
                    ParameterType::Table,
                    Direction::Export,
                    548,
                    1088,
                    false
                ),
                (
                    "EV_LIVE_HASH",
                    ParameterType::Char,
                    Direction::Export,
                    64,
                    128,
                    false
                ),
            ],
            "wide: {wide}"
        );
        assert_eq!(
            fixture.counter(b"fake_creates\0"),
            0,
            "a lookup creates nothing"
        );
    }
}

#[test]
fn an_absent_function_is_none_and_any_other_lookup_failure_is_an_error() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);

        assert_eq!(fixture.api.describe_function("MISSING_FM").unwrap(), None);

        let error = fixture.api.describe_function("OTHER_FM").unwrap_err();

        assert!(
            matches!(&error, TransportError::Protocol(message)
                if message.contains("RfcGetFunctionDesc") && message.contains("bad function")),
            "{error:?}"
        );
        // A description of a function without parameter metadata fails, never reads as absent.
        let error = fixture
            .api
            .describe_function("SADT_REST_RFC_ENDPOINT")
            .unwrap_err();

        assert!(
            error.to_string().contains("RfcGetParameterCount"),
            "{error:?}"
        );
    }
}

#[test]
fn null_handles_from_the_library_are_errors_and_the_function_is_still_destroyed() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);
        let error = fixture
            .api
            .call_function(&call_named("NO_SUCH"))
            .unwrap_err();

        assert!(
            error.to_string().contains("RfcGetFunctionDesc"),
            "{error:?}"
        );

        let error = fixture
            .api
            .call_function(&call_named("NULL_CREATE"))
            .unwrap_err();

        assert!(error.to_string().contains("RfcCreateFunction"), "{error:?}");

        let mut call = call_named("NULL_ROW");

        call.export_tables.push(TableRead {
            name: "ET_MESSAGES".to_owned(),
            columns: vec![column("TYPE", CellKind::Chars(1))],
        });

        let error = fixture.api.call_function(&call).unwrap_err();

        assert!(error.to_string().contains("RfcGetCurrentRow"), "{error:?}");
        assert!(
            error.was_sent(),
            "a read after the invoke is post-send: {error:?}"
        );
        // Created: NULL_CREATE and NULL_ROW; the null create has nothing to destroy.
        assert_eq!(fixture.counter(b"fake_creates\0"), 2);
        assert_eq!(fixture.counter(b"fake_destroys\0"), 1);
    }
}

#[test]
fn input_that_is_over_length_or_malformed_never_reaches_the_library() {
    let mut fixture = Fixture::new(true, &[]);
    let long = "x".repeat(256);
    let mut cases = vec![
        deploy_call("YTEST1", &long, &[]),
        deploy_call("YTEST1", "READ", &[long.as_str()]),
        call_named(""),
        call_named("HAS BLANK"),
        call_named(&"F".repeat(31)),
    ];

    let mut bad_field = deploy_call("YTEST1", "READ", &[]);

    bad_field.imports.push(("".to_owned(), "x".to_owned()));
    cases.push(bad_field);

    let mut bad_column = deploy_call("YTEST1", "READ", &[]);

    bad_column
        .exports
        .push(column("EV_STATUS", CellKind::Chars(0)));
    cases.push(bad_column);

    let mut wide_column = deploy_call("YTEST1", "READ", &[]);

    wide_column
        .exports
        .push(column("EV_STATUS", CellKind::Chars(256)));
    cases.push(wide_column);

    let mut bad_table = deploy_call("YTEST1", "READ", &[]);

    bad_table.tables[0].rows.push(pairs(&[("LI NE", "x")]));
    cases.push(bad_table);

    for call in cases {
        let error = fixture.api.call_function(&call).unwrap_err();

        assert!(
            matches!(&error, TransportError::Protocol(message) if message.starts_with("RFC call: ")),
            "{error:?}"
        );
        assert!(!error.was_sent());
    }

    let error = fixture.api.describe_function("HAS BLANK").unwrap_err();

    assert!(matches!(error, TransportError::Protocol(_)), "{error:?}");
    assert_eq!(fixture.counter(b"fake_creates\0"), 0);
    assert_eq!(fixture.counter(b"fake_sets\0"), 0);
}

#[test]
fn a_library_that_refuses_a_value_or_a_wrong_typed_access_reports_an_error_in_either_width() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);

        // Longer than the field IV_MODE (10 characters): the library refuses.
        let error = fixture
            .api
            .call_function(&deploy_call("YTEST1", "ELEVENCHARS", &[]))
            .unwrap_err();

        assert!(error.to_string().contains("RfcSetChars"), "{error:?}");
        assert!(error.to_string().contains("buffer too small"), "{error:?}");
        assert!(!error.was_sent());

        // A table name used as a field, and a field name used as a table.
        let mut as_field = deploy_call("YTEST1", "READ", &[]);

        as_field
            .imports
            .push(("IT_SOURCE".to_owned(), "x".to_owned()));
        assert!(
            fixture
                .api
                .call_function(&as_field)
                .unwrap_err()
                .to_string()
                .contains("wrong type")
        );

        let mut as_table = deploy_call("YTEST1", "READ", &[]);

        as_table.tables.push(TableWrite {
            name: "IV_MODE".to_owned(),
            rows: Vec::new(),
        });
        assert!(
            fixture
                .api
                .call_function(&as_table)
                .unwrap_err()
                .to_string()
                .contains("RfcGetTable")
        );

        // A character field read as INT4, an INT4 field read as characters, and a buffer that is
        // shorter than the value.
        let mut as_int = deploy_call("YTEST1", "READ", &[]);

        as_int.exports = vec![column("EV_STATUS", CellKind::Int)];
        as_int.export_tables.clear();

        let error = fixture.api.call_function(&as_int).unwrap_err();

        assert!(error.to_string().contains("RfcGetInt"), "{error:?}");
        assert!(error.was_sent(), "{error:?}");

        let mut as_chars = deploy_call("YTEST1", "READ", &[]);

        as_chars.exports.clear();
        as_chars.export_tables = vec![TableRead {
            name: "ET_MESSAGES".to_owned(),
            columns: vec![column("ROW", CellKind::Chars(5))],
        }];
        assert!(
            fixture
                .api
                .call_function(&as_chars)
                .unwrap_err()
                .to_string()
                .contains("RfcGetChars")
        );

        let mut short = deploy_call("YTEST1", "READ", &[]);

        short.exports = vec![column("EV_LIVE_HASH", CellKind::Chars(4))];
        short.export_tables.clear();

        let error = fixture.api.call_function(&short).unwrap_err();

        assert!(error.to_string().contains("buffer too small"), "{error:?}");
    }
}

#[test]
fn a_table_that_claims_more_rows_than_the_limit_is_an_error_not_an_allocation() {
    let mut fixture = Fixture::new(true, &[]);
    let mut call = call_named("HUGE_TABLE");

    call.export_tables.push(TableRead {
        name: "ET_MESSAGES".to_owned(),
        columns: vec![column("TYPE", CellKind::Chars(1))],
    });

    let error = fixture.api.call_function(&call).unwrap_err();

    assert!(error.is_post_send_uncertain(), "{error:?}");
    assert!(error.to_string().contains("1000000 rows"), "{error:?}");
}

#[test]
fn a_library_without_the_ext_symbols_still_serves_sadt() {
    for wide in WIDTHS {
        let all = RfcSymbols::optional_names();
        let mut fixture = Fixture::new(wide, &all);
        let message = fixture.api.function_support().unwrap_err();

        for name in &all {
            assert!(message.contains(name), "{message}");
        }

        assert!(message.contains("function module calls"), "{message}");
        assert!(
            message.contains("ADT and SADT are not affected"),
            "{message}"
        );
        assert_eq!(
            fixture
                .api
                .call_function(&deploy_call("YTEST1", "READ", &[]))
                .unwrap_err(),
            TransportError::Protocol(message.clone())
        );
        assert_eq!(
            fixture
                .api
                .describe_function("YTEST_FAKE_FUNCTION")
                .unwrap_err(),
            TransportError::Protocol(message)
        );
        assert_eq!(fixture.counter(b"fake_creates\0"), 0);

        let response = fixture
            .api
            .invoke(RfcEndpointRequest {
                function: "SADT_REST_RFC_ENDPOINT",
                method: "GET".to_owned(),
                uri: "/sap/bc/adt/discovery".to_owned(),
                version: "HTTP/1.1".to_owned(),
                headers: BTreeMap::from([("Accept".to_owned(), "text/plain".to_owned())]),
                body: Vec::new(),
            })
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        assert_eq!(
            (response.status, response.body.as_slice()),
            (200, &b"ok"[..])
        );
    }
}

#[test]
fn one_missing_ext_symbol_disables_the_whole_ext_path_and_names_only_that_symbol() {
    let mut fixture = Fixture::new(true, &["RfcGetRowCount"]);
    let message = fixture.api.function_support().unwrap_err();

    assert!(message.contains("RfcGetRowCount"), "{message}");
    assert!(!message.contains("RfcMoveTo"), "{message}");
    assert!(
        fixture
            .api
            .call_function(&deploy_call("YTEST1", "READ", &[]))
            .is_err()
    );
    assert_eq!(fixture.counter(b"fake_creates\0"), 0);
}

#[test]
fn a_library_with_the_ext_symbols_reports_support_and_sadt_is_unchanged() {
    let mut fixture = Fixture::new(true, &[]);

    assert_eq!(fixture.api.function_support(), Ok(()));
    assert_eq!(
        fixture
            .api
            .invoke(RfcEndpointRequest {
                function: "SADT_REST_RFC_ENDPOINT",
                method: "GET".to_owned(),
                uri: "/sap/bc/adt/discovery".to_owned(),
                version: "HTTP/1.1".to_owned(),
                headers: BTreeMap::from([("Accept".to_owned(), "text/plain".to_owned())]),
                body: Vec::new(),
            })
            .unwrap()
            .status,
        200
    );
}

// ---- the typed RFC_READ_TABLE read -------------------------------------------------------------

use abapreach::transport::TableCall;

fn read_call(table: &str, fields: &[&str], rows: u32) -> TableCall {
    TableCall::for_test(table, fields, &["SEQNO = '000'"], rows).unwrap()
}

fn fail_next_create(path: &Path) {
    let library = unsafe { libloading::Library::new(path) }.unwrap();
    let arm: libloading::Symbol<unsafe extern "C" fn() -> u32> =
        unsafe { library.get(b"fake_fail_next_create\0") }.unwrap();

    unsafe { arm() };
}

#[test]
fn the_table_read_cuts_a_row_wider_than_the_generic_call_allows_in_either_width() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);
        let call = read_call("SNAP", &["SEQNO", "FLIST", "FLIST02"], 2);
        let rows = fixture
            .api
            .read_table(&call)
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        assert_eq!(rows.columns, ["SEQNO", "FLIST", "FLIST02"], "wide: {wide}");
        // 403 characters: the row buffer is 512 wide, which the generic call would refuse.
        assert_eq!(
            rows.rows,
            [
                ["000".to_owned(), "FC007X_ERROR".to_owned(), "x".repeat(200)],
                ["001".to_owned(), "FC007X_ERROR".to_owned(), "x".repeat(200)],
            ],
            "wide: {wide}"
        );
        // QUERY_TABLE, ROWCOUNT, three field rows and one condition line reached the library.
        assert_eq!(fixture.counter(b"fake_sets\0"), 6, "wide: {wide}");
        assert_eq!(fixture.counter(b"fake_creates\0"), 1);
        assert_eq!(fixture.counter(b"fake_destroys\0"), 1);

        // ROWCOUNT is what the library sees: a larger one gives what the fake has, three rows.
        let more = fixture
            .api
            .read_table(&read_call("SNAP", &["SEQNO"], 50))
            .unwrap();

        assert_eq!(more.rows.len(), 3, "wide: {wide}");
    }
}

#[test]
fn a_table_read_that_fails_at_any_stage_is_an_error_and_leaves_no_function_behind() {
    for wide in WIDTHS {
        // Setting an import, the invoke and reading a row each fail on their own.
        for (table, needle) in [
            ("ZFAIL_SET", "RfcSetChars"),
            ("ZFAIL_INVOKE", "[TABLE_NOT_AVAILABLE]"),
            ("ZFAIL_READ", "RfcGetChars"),
        ] {
            let mut fixture = Fixture::new(wide, &[]);
            let error = fixture
                .api
                .read_table(&read_call(table, &["SEQNO", "FLIST"], 3))
                .unwrap_err();

            assert!(
                error.to_string().contains(needle),
                "{table} wide {wide}: {error}"
            );
            assert_eq!(
                fixture.counter(b"fake_destroys\0"),
                fixture.counter(b"fake_creates\0"),
                "{table} wide {wide}: the function is destroyed"
            );
        }

        // The creation fails: there is no function to destroy, and the next call works.
        let mut fixture = Fixture::new(wide, &[]);

        fail_next_create(&fixture.path);

        let error = fixture
            .api
            .read_table(&read_call("SNAP", &["SEQNO"], 1))
            .unwrap_err();

        assert!(
            error.to_string().contains("cannot create"),
            "wide {wide}: {error}"
        );
        assert_eq!(fixture.counter(b"fake_destroys\0"), 0);
        assert!(
            fixture
                .api
                .read_table(&read_call("SNAP", &["SEQNO"], 1))
                .is_ok()
        );
        assert_eq!(fixture.counter(b"fake_destroys\0"), 1);
    }
}

#[test]
fn a_library_without_the_function_symbols_cannot_read_a_table() {
    let mut fixture = Fixture::new(true, &["RfcGetChars"]);
    let error = fixture
        .api
        .read_table(&read_call("SNAP", &["SEQNO"], 1))
        .unwrap_err();

    assert!(error.to_string().contains("RfcGetChars"), "{error}");
    assert_eq!(fixture.counter(b"fake_creates\0"), 0);
}

// ---- the typed RFC_SYSTEM_INFO call ------------------------------------------------------------

fn sysinfo_fail(path: &Path, stage: u32) {
    let library = unsafe { libloading::Library::new(path) }.unwrap();
    let arm: libloading::Symbol<unsafe extern "C" fn(u32) -> u32> =
        unsafe { library.get(b"fake_sysinfo_fail\0") }.unwrap();

    unsafe { arm(stage) };
}

/// A failed call leaves nothing behind: every function it created is destroyed.
fn assert_clean(fixture: &Fixture, context: &str) {
    assert_eq!(
        fixture.counter(b"fake_destroys\0"),
        fixture.counter(b"fake_creates\0"),
        "{context}: the function is destroyed"
    );
}

#[test]
fn the_system_info_call_names_the_function_and_the_structure_and_trims_the_fields() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);
        let info = fixture
            .api
            .system_info()
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        // The fake knows only `RFC_SYSTEM_INFO` for the function and `RFCSI_EXPORT` for the
        // structure, and pads every field to its width: the trailing blanks are gone.
        assert_eq!(info.instance, "apphost_HD1_00", "wide: {wide}");
        assert_eq!(info.host, "apphost", "wide: {wide}");
        assert_eq!(info.system_id, "HD1", "wide: {wide}");
        assert_eq!(info.release, "750", "wide: {wide}");
        assert_eq!(info.kernel, "753", "wide: {wide}");
        assert_eq!(info.database, "DB6", "wide: {wide}");
        assert!(info.invoke_time.is_some(), "wide: {wide}");
        assert_eq!(fixture.counter(b"fake_creates\0"), 1, "wide: {wide}");
        assert_eq!(fixture.counter(b"fake_destroys\0"), 1, "wide: {wide}");
        // One structure read, and each of the six fields sized once and read once.
        assert_eq!(fixture.counter(b"fake_structure_gets\0"), 1, "wide: {wide}");
        assert_eq!(fixture.counter(b"fake_string_lengths\0"), 6, "wide: {wide}");
        assert_eq!(fixture.counter(b"fake_string_gets\0"), 6, "wide: {wide}");
        // No parameter was set: the call sends nothing of its own.
        assert_eq!(fixture.counter(b"fake_sets\0"), 0, "wide: {wide}");
    }
}

#[test]
fn the_system_info_buffers_are_the_reported_length_plus_the_terminator_in_either_width() {
    // The fake refuses a buffer of any other size, so a successful read is the proof; a second
    // call on the same connection reads the same values and keeps the handles balanced.
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);
        let first = fixture.api.system_info().unwrap();
        let second = fixture.api.system_info().unwrap();

        assert_eq!(
            (first.instance.as_str(), first.release.as_str()),
            (second.instance.as_str(), second.release.as_str()),
            "wide: {wide}"
        );
        assert_eq!(fixture.counter(b"fake_creates\0"), 2, "wide: {wide}");
        assert_eq!(fixture.counter(b"fake_destroys\0"), 2, "wide: {wide}");
    }
}

#[test]
fn a_system_info_call_that_fails_at_any_stage_is_an_error_and_leaves_no_function_behind() {
    for wide in WIDTHS {
        // The invoke, the structure, a string read and a string length each fail on their own.
        for (stage, needle) in [
            (1, "RfcInvoke: system info invoke failed"),
            (2, "RfcGetStructure"),
            (3, "RfcGetString"),
            (5, "RfcGetStringLength"),
        ] {
            let mut fixture = Fixture::new(wide, &[]);

            sysinfo_fail(&fixture.path, stage);

            let error = fixture.api.system_info().unwrap_err();

            assert!(
                error.to_string().contains(needle),
                "stage {stage} wide {wide}: {error}"
            );
            assert_eq!(
                fixture.counter(b"fake_creates\0"),
                1,
                "stage {stage} wide {wide}"
            );
            assert_clean(&fixture, &format!("stage {stage} wide {wide}"));
        }

        // The creation fails: there is no function to destroy, and the next call works.
        let mut fixture = Fixture::new(wide, &[]);

        fail_next_create(&fixture.path);

        let error = fixture.api.system_info().unwrap_err();

        assert!(
            error.to_string().contains("cannot create"),
            "wide {wide}: {error}"
        );
        assert_eq!(fixture.counter(b"fake_destroys\0"), 0, "wide: {wide}");
        assert!(fixture.api.system_info().is_ok(), "wide: {wide}");
        assert_eq!(fixture.counter(b"fake_destroys\0"), 1, "wide: {wide}");
    }
}

#[test]
fn a_host_that_cannot_be_read_only_drops_the_host() {
    for wide in WIDTHS {
        let mut fixture = Fixture::new(wide, &[]);

        sysinfo_fail(&fixture.path, 4);

        let info = fixture
            .api
            .system_info()
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        assert_eq!(info.host, "", "wide: {wide}");
        assert_eq!(info.instance, "apphost_HD1_00", "wide: {wide}");
        assert_eq!(info.release, "750", "wide: {wide}");
        assert_clean(&fixture, &format!("wide {wide}"));
    }
}

#[test]
fn the_system_info_call_needs_no_optional_symbol() {
    for wide in WIDTHS {
        let all = RfcSymbols::optional_names();
        let mut fixture = Fixture::new(wide, &all);
        let info = fixture
            .api
            .system_info()
            .unwrap_or_else(|error| panic!("wide {wide}: {error}"));

        assert_eq!(info.system_id, "HD1", "wide: {wide}");
        assert_clean(&fixture, &format!("wide {wide}"));
    }
}

#[test]
fn a_library_without_a_symbol_the_call_reads_with_never_reaches_system_info() {
    for wide in WIDTHS {
        for symbol in [
            "RfcGetString",
            "RfcGetStringLength",
            "RfcGetStructure",
            "RfcInvoke",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = build_without_base(directory.path(), wide, &[], &[symbol]);
            let library = RfcLibrary::load(&Env::test_paths(path.to_str().unwrap(), "", ""))
                .unwrap_or_else(|error| panic!("the fake must load: {error}"));
            let parameters = vec![("ASHOST".to_owned(), "good.example.com".to_owned())];
            let error = match DynamicRfcApi::load(library, parameters) {
                Ok(_) => panic!("{symbol} wide {wide}: the API must not load"),
                Err(error) => error,
            };

            assert!(error.contains(symbol), "{symbol} wide {wide}: {error}");
            assert_eq!(
                counter_at(&path, b"fake_creates\0"),
                0,
                "{symbol} wide {wide}"
            );
        }
    }
}

use abapreach::transport::rfc::{
    CandidateFs, Env, MAX_GUI_SEARCH_DEPTH, Platform, RFC_TRACE_DIR_ENV, RfcLibrary, TraceTarget,
    apply_trace_directory, bundle_version, from_sap_uc, gui_library_filename, rfc_library_filename,
    sap_uc, sdk_candidates, trace_directory, trace_environment,
};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The trace directory the fake `RfcSetTraceDir` was called with.
static TRACE_CALLS: Mutex<Option<String>> = Mutex::new(None);

unsafe extern "C" fn record_trace_dir(directory: *const u16, _: *mut c_void) -> i32 {
    let mut units = Vec::new();

    // The pointer is NUL-terminated UTF-16 that stays alive for the duration of the call.
    unsafe {
        while *directory.add(units.len()) != 0 {
            units.push(*directory.add(units.len()));
        }
    }

    *TRACE_CALLS.lock().unwrap() = Some(from_sap_uc(&units).unwrap());

    0
}

/// The file-system queries candidate discovery needs; every platform is exercised on any OS.
struct FakeFs {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl FakeFs {
    fn new(files: &[&Path], directories: &[&Path]) -> Self {
        Self {
            files: files.iter().map(|file| file.to_path_buf()).collect(),
            directories: directories
                .iter()
                .map(|directory| directory.to_path_buf())
                .collect(),
        }
    }

    fn from_paths(files: &[PathBuf], directories: &[PathBuf]) -> Self {
        Self {
            files: files.to_vec(),
            directories: directories.to_vec(),
        }
    }
}

impl CandidateFs for FakeFs {
    fn is_file(&self, path: &Path) -> bool {
        self.files.iter().any(|file| file == path)
    }

    fn child_directories(&self, path: &Path) -> Vec<PathBuf> {
        let mut children = self
            .directories
            .iter()
            .filter(|directory| directory.parent() == Some(path))
            .cloned()
            .collect::<Vec<_>>();

        children.sort();
        children
    }
}

fn env_with(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn sdk_library(platform: Platform, root: impl AsRef<Path>) -> PathBuf {
    root.as_ref()
        .join("lib")
        .join(rfc_library_filename(platform))
}

fn sdk_libraries(platform: Platform, roots: &[&Path]) -> Vec<PathBuf> {
    roots
        .iter()
        .map(|root| sdk_library(platform, root))
        .collect()
}

/// A SAP GUI bundle `levels` directory levels below `/Applications`, plus the directories the
/// search walks: level 0 is a bundle directly in `/Applications`.
fn bundle_below_applications(levels: usize) -> (PathBuf, Vec<PathBuf>) {
    let mut directories = vec![Path::new("/Applications").to_path_buf()];

    for level in 1..levels {
        let parent = directories.last().unwrap().clone();

        directories.push(parent.join(format!("level{level}")));
    }

    let bundle = directories
        .last()
        .unwrap()
        .join("SAP GUI for Java 8.10.app");

    directories.push(bundle.clone());

    (bundle, directories)
}

fn fake_fs(files: &[PathBuf], directories: &[PathBuf]) -> FakeFs {
    let file_refs = files.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let directory_refs = directories.iter().map(PathBuf::as_path).collect::<Vec<_>>();

    FakeFs::new(&file_refs, &directory_refs)
}

fn macos_bundle(name: &str) -> PathBuf {
    Path::new("/Applications").join(name)
}

/// Both libraries of a macOS SAP GUI bundle, in search order.
fn macos_gui_libraries(bundle: &Path) -> (PathBuf, PathBuf) {
    (
        bundle
            .join("Contents")
            .join("Resources")
            .join("bin")
            .join("unicode")
            .join("libsapnwrfc.dylib"),
        bundle
            .join("Contents")
            .join("Resources")
            .join("bin")
            .join("libsapnwrfccm.dylib"),
    )
}

fn both_libraries(bundle: &Path) -> Vec<PathBuf> {
    let (rfc, cm) = macos_gui_libraries(bundle);

    vec![rfc, cm]
}

fn applications_with(bundles: &[&PathBuf]) -> Vec<PathBuf> {
    std::iter::once(Path::new("/Applications").to_path_buf())
        .chain(bundles.iter().map(|bundle| (*bundle).clone()))
        .collect()
}

#[test]
fn candidate_paths_preserve_override_gui_sdk_order() {
    let env = Env::test_paths("/override", "/gui", "/sdk");

    assert_eq!(
        RfcLibrary::candidate_paths(&env),
        vec![
            PathBuf::from("/override"),
            PathBuf::from("/gui"),
            PathBuf::from("/sdk"),
        ]
    );
}

#[test]
fn sap_uc_is_nul_terminated_utf16_and_rejects_invalid_units() {
    let units = sap_uc("Aé");

    assert_eq!(units, vec![65, 233, 0]);
    assert_eq!(from_sap_uc(&units).unwrap(), "Aé");
    assert!(from_sap_uc(&[0xd800, 0]).is_err());
}

#[test]
fn the_trace_goes_to_the_temp_dir_by_symbol_or_by_environment() {
    let temp = std::env::temp_dir();

    assert_eq!(RFC_TRACE_DIR_ENV, "RFC_TRACE_DIR");
    assert_eq!(trace_directory(), temp);
    assert_eq!(
        trace_environment(),
        (RFC_TRACE_DIR_ENV, temp.display().to_string())
    );

    assert_eq!(apply_trace_directory(None), TraceTarget::Environment);
    assert_eq!(
        apply_trace_directory(Some(record_trace_dir)),
        TraceTarget::Symbol
    );
    assert_eq!(
        TRACE_CALLS.lock().unwrap().clone(),
        Some(temp.display().to_string())
    );
}

#[test]
fn platform_library_names_match_the_documented_files() {
    assert_eq!(rfc_library_filename(Platform::Windows), "sapnwrfc.dll");
    assert_eq!(gui_library_filename(Platform::Windows), "sapnwrfccm.dll");
    assert_eq!(rfc_library_filename(Platform::MacOs), "libsapnwrfc.dylib");
    assert_eq!(gui_library_filename(Platform::MacOs), "libsapnwrfccm.dylib");
    assert_eq!(rfc_library_filename(Platform::Linux), "libsapnwrfc.so");
    assert_eq!(gui_library_filename(Platform::Linux), "libsapnwrfccm.so");
}

#[test]
fn current_platform_matches_the_build_target() {
    let expected = if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Linux
    };

    assert_eq!(Platform::current(), expected);
}

#[test]
fn windows_gui_and_sdk_paths_use_program_files_and_userprofile() {
    let program_files = Path::new(r"C:\Program Files");
    let home = Path::new(r"C:\Users\DEV");
    let env = env_with(&[
        ("USERPROFILE", r"C:\Users\DEV"),
        ("ProgramFiles", r"C:\Program Files"),
    ]);
    let gui = program_files
        .join("SAP")
        .join("FrontEnd")
        .join("SAPGUI")
        .join("sapnwrfccm.dll");
    let fs = FakeFs::new(&[&gui], &[]);
    let discovered = Env::discover(Platform::Windows, &env, &fs);
    let sdk = sdk_libraries(
        Platform::Windows,
        &[
            &home.join("sap").join("nwrfcsdk"),
            &program_files.join("SAP").join("nwrfcsdk"),
        ],
    );

    assert_eq!(
        RfcLibrary::candidate_paths(&discovered),
        vec![gui].into_iter().chain(sdk).collect::<Vec<_>>()
    );
}

#[test]
fn sdk_roots_start_with_sapnwrfc_home_then_the_platform_standard_paths() {
    let env = env_with(&[("SAPNWRFC_HOME", "/custom/nwrfcsdk"), ("HOME", "/home/DEV")]);
    let roots = [
        Path::new("/custom/nwrfcsdk"),
        &Path::new("/home/DEV").join("sap").join("nwrfcsdk"),
        Path::new("/opt/sap/nwrfcsdk"),
        Path::new("/usr/local/sap/nwrfcsdk"),
    ];

    for platform in [Platform::MacOs, Platform::Linux] {
        assert_eq!(
            sdk_candidates(platform, &env),
            sdk_libraries(platform, &roots),
            "unexpected SDK order for {platform:?}"
        );
    }

    // Windows keeps `USERPROFILE` as the home fallback and never uses the Unix standard roots.
    let windows = env_with(&[("USERPROFILE", r"C:\Users\DEV")]);

    assert_eq!(
        sdk_candidates(Platform::Windows, &windows),
        sdk_libraries(
            Platform::Windows,
            &[
                &Path::new(r"C:\Users\DEV").join("sap").join("nwrfcsdk"),
                &Path::new(r"C:\Program Files").join("SAP").join("nwrfcsdk"),
            ]
        )
    );

    // A home that no variable provides contributes no root at all.
    let unix_roots = [
        Path::new("/opt/sap/nwrfcsdk"),
        Path::new("/usr/local/sap/nwrfcsdk"),
    ];

    assert_eq!(
        sdk_candidates(Platform::Linux, &env_with(&[])),
        sdk_libraries(Platform::Linux, &unix_roots)
    );
}

#[test]
fn macos_gui_candidates_include_both_library_names() {
    let bundle = macos_bundle("SAPGUI 8.10.app");
    let libraries = both_libraries(&bundle);
    let files = [libraries[0].as_path(), libraries[1].as_path()];
    let fs = FakeFs::new(&files, &[Path::new("/Applications"), &bundle]);
    let discovered = Env::discover(Platform::MacOs, &env_with(&[("HOME", "/Users/DEV")]), &fs);
    let sdk = sdk_libraries(
        Platform::MacOs,
        &[
            &Path::new("/Users/DEV").join("sap").join("nwrfcsdk"),
            Path::new("/opt/sap/nwrfcsdk"),
            Path::new("/usr/local/sap/nwrfcsdk"),
        ],
    );

    assert_eq!(
        RfcLibrary::candidate_paths(&discovered),
        libraries.into_iter().chain(sdk).collect::<Vec<_>>()
    );
    assert!(discovered.notes().is_empty());
}

#[test]
fn bundle_version_reads_the_first_number_in_the_name_only() {
    let cases = [
        ("SAP GUI for Java 8.10.app", vec![8, 10]),
        ("SAP GUI for Java 8.10 rev15.app", vec![8, 10]),
        ("SAPGUI 10.0.app", vec![10, 0]),
        ("SAP GUI for Java.app", Vec::new()),
    ];

    for (name, expected) in cases {
        assert_eq!(
            bundle_version(&macos_bundle(name)),
            expected,
            "unexpected version for {name}"
        );
    }
}

#[test]
fn candidate_selection_uses_highest_unloaded_gui_version() {
    let ranked = [
        macos_bundle("SAP GUI for Java 7.70.app"),
        macos_bundle("SAP GUI for Java 8.10 rev15.app"),
        macos_bundle("SAP GUI for Java 10.0.app"),
    ];
    let libraries = ranked.iter().flat_map(|bundle| both_libraries(bundle));
    let files = libraries.collect::<Vec<_>>();
    let fs = FakeFs::from_paths(
        &files,
        &applications_with(&ranked.iter().collect::<Vec<_>>()),
    );
    let discovered = Env::discover(Platform::MacOs, &env_with(&[]), &fs);
    let candidates = RfcLibrary::candidate_paths(&discovered);
    let expected = ranked
        .iter()
        .rev()
        .flat_map(|bundle| both_libraries(bundle))
        .collect::<Vec<_>>();

    // Numeric order, not lexical order: 10.0 outranks 8.10, and 8.10 outranks 7.70.
    assert_eq!(candidates[..expected.len()].to_vec(), expected);
}

#[test]
fn a_gui_bundle_without_a_version_number_ranks_last() {
    let versioned = macos_bundle("SAP GUI for Java 7.70.app");
    let unversioned = macos_bundle("SAPGUI.app");
    let files = both_libraries(&versioned)
        .into_iter()
        .chain(both_libraries(&unversioned))
        .collect::<Vec<_>>();
    let fs = FakeFs::from_paths(&files, &applications_with(&[&versioned, &unversioned]));
    let discovered = Env::discover(Platform::MacOs, &env_with(&[]), &fs);
    let candidates = RfcLibrary::candidate_paths(&discovered);

    assert_eq!(candidates[..2].to_vec(), both_libraries(&versioned));
}

#[test]
fn a_gui_bundle_at_the_search_limit_is_found() {
    let (bundle, directories) = bundle_below_applications(MAX_GUI_SEARCH_DEPTH);
    let libraries = both_libraries(&bundle);
    let fs = fake_fs(&libraries, &directories);
    let discovered = Env::discover(Platform::MacOs, &env_with(&[]), &fs);

    assert_eq!(
        RfcLibrary::candidate_paths(&discovered)[..2].to_vec(),
        libraries
    );
}

#[test]
fn a_gui_bundle_below_the_search_limit_is_outside_the_search() {
    let (bundle, directories) = bundle_below_applications(MAX_GUI_SEARCH_DEPTH + 1);
    let libraries = both_libraries(&bundle);
    let fs = fake_fs(&libraries, &directories);
    let discovered = Env::discover(Platform::MacOs, &env_with(&[]), &fs);
    let candidates = RfcLibrary::candidate_paths(&discovered);

    assert!(candidates.iter().all(|path| !path.starts_with(&bundle)));
    assert_eq!(
        candidates,
        sdk_candidates(Platform::MacOs, &env_with(&[])),
        "the walk must stop at the limit and leave only the SDK family"
    );
}

#[test]
fn linux_gui_package_layout_precedes_the_nw_rfc_sdk_family() {
    let root = Path::new("/opt/sapgui");
    let cm = root.join("bin").join("libsapnwrfccm.so");
    let rfc = root.join("bin").join("unicode").join("libsapnwrfc.so");
    let icu = root.join("bin").join("unicode").join("libicuuc.so.50");
    let fs = FakeFs::new(&[&cm, &rfc, &icu], &[]);
    let discovered = Env::discover(Platform::Linux, &env_with(&[("HOME", "/home/DEV")]), &fs);
    let sdk = sdk_libraries(
        Platform::Linux,
        &[
            &Path::new("/home/DEV").join("sap").join("nwrfcsdk"),
            Path::new("/opt/sap/nwrfcsdk"),
            Path::new("/usr/local/sap/nwrfcsdk"),
        ],
    );

    assert_eq!(
        RfcLibrary::candidate_paths(&discovered),
        vec![cm, rfc].into_iter().chain(sdk).collect::<Vec<_>>()
    );
}

#[test]
fn windows_32_bit_gui_installation_is_skipped_with_a_reason() {
    let library = Path::new(r"C:\Program Files (x86)")
        .join("SAP")
        .join("FrontEnd")
        .join("SAPGUI")
        .join("sapnwrfccm.dll");
    let fs = FakeFs::new(&[&library], &[]);
    let discovered = Env::discover(
        Platform::Windows,
        &env_with(&[("USERPROFILE", r"C:\Users\DEV")]),
        &fs,
    );
    let candidates = RfcLibrary::candidate_paths(&discovered);

    assert!(!candidates.contains(&library));
    assert_eq!(candidates.len(), 2);
    assert_eq!(discovered.notes().len(), 1);

    let note = &discovered.notes()[0];

    assert!(note.contains("32-bit"), "unexpected reason: {note}");
    assert!(
        note.contains(&library.display().to_string()),
        "the reason must name the skipped library: {note}"
    );
}

#[test]
fn automatic_candidate_order_is_override_then_gui_then_sdk() {
    let bundle = macos_bundle("SAPGUI 8.10.app");
    let libraries = both_libraries(&bundle);
    let files = [libraries[0].as_path(), libraries[1].as_path()];
    let fs = FakeFs::new(&files, &[Path::new("/Applications"), &bundle]);
    let env = env_with(&[
        ("ABAPREACH_RFC_LIBRARY", "/custom/libsapnwrfc.dylib"),
        ("HOME", "/Users/DEV"),
    ]);
    let discovered = Env::discover(Platform::MacOs, &env, &fs);
    let candidates = RfcLibrary::candidate_paths(&discovered);

    assert_eq!(candidates[0], PathBuf::from("/custom/libsapnwrfc.dylib"));
    assert_eq!(candidates[1..3].to_vec(), libraries);
    assert!(
        candidates[3..]
            .iter()
            .all(|path| path.ends_with("nwrfcsdk/lib/libsapnwrfc.dylib"))
    );
}

#[test]
fn the_running_environment_always_offers_the_platform_sdk_roots() {
    let candidates = RfcLibrary::candidate_paths(&Env::current());
    let library = Path::new("lib").join(rfc_library_filename(Platform::current()));

    assert!(
        !candidates.is_empty(),
        "the automatic SDK candidates are missing: {candidates:?}"
    );
    assert!(candidates.iter().all(|path| path.is_absolute()));
    assert!(
        candidates.iter().any(|path| path.ends_with(&library)),
        "no SDK candidate for the running platform: {candidates:?}"
    );
}

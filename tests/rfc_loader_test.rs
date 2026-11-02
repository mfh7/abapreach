use abapreach::transport::rfc::{
    Arch, Candidate, CandidateFs, ELF_MAGIC, ElfDynamic, Env, Family,
    LOAD_WITH_ALTERED_SEARCH_PATH, LibraryIdentity, LoadError, MAX_GUI_SEARCH_DEPTH,
    MAX_READ_BYTES, Origin, PE_OFFSET_END, Platform, RFC_TRACE_DIR_ENV, RfcLibrary, SystemFs,
    TraceTarget, UcString, UcWidth, apply_trace_directory, apply_trace_directory_to,
    bundle_version, compatible_arch, decide, declared_archs, dependency_preload, elf_dynamic,
    family_of, from_sap_uc, gui_library_filename, icu_dependencies, install_version,
    load_compatible, preload_dependencies, rfc_library_filename, sap_uc, sdk_candidates,
    trace_directory, trace_environment, windows_dependency_preload, windows_load_flags,
};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The trace directory the fake `RfcSetTraceDir` was called with.
static TRACE_CALLS: Mutex<Option<String>> = Mutex::new(None);

unsafe extern "C" fn record_trace_dir(directory: *const c_void, _: *mut c_void) -> i32 {
    let directory = directory.cast::<u16>();
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

/// `RfcSetTraceDir` failing, as SAP does with `RFC_INVALID_PARAMETER`.
const RFC_TRACE_FAILURE: i32 = 20;

/// The tests above share `TRACE_CALLS`, so they run one at a time.
static TRACE_LOCK: Mutex<()> = Mutex::new(());

/// The same call as a single-byte library reads it: a `char*`, one byte per character.
unsafe extern "C" fn record_narrow_trace_dir(directory: *const c_void, _: *mut c_void) -> i32 {
    // The pointer is a NUL-terminated byte string that stays alive for the duration of the call.
    let text = unsafe { std::ffi::CStr::from_ptr(directory.cast()) };

    *TRACE_CALLS.lock().unwrap() = Some(text.to_str().unwrap().to_owned());

    0
}

unsafe extern "C" fn fail_trace_dir(directory: *const c_void, error: *mut c_void) -> i32 {
    unsafe { record_trace_dir(directory, error) };

    RFC_TRACE_FAILURE
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
fn the_trace_directory_is_the_os_temp_dir() {
    let temp = std::env::temp_dir();

    assert_eq!(RFC_TRACE_DIR_ENV, "RFC_TRACE_DIR");
    assert_eq!(trace_directory(), temp);
    assert_eq!(
        trace_environment(),
        (RFC_TRACE_DIR_ENV, temp.display().to_string())
    );
}

#[test]
fn trace_symbol_exported_and_successful_sets_the_temp_dir() {
    let _guard = TRACE_LOCK.lock().unwrap();
    *TRACE_CALLS.lock().unwrap() = None;

    assert_eq!(
        apply_trace_directory(Some(record_trace_dir), UcWidth::Utf16),
        TraceTarget::Symbol
    );
    assert_eq!(
        TRACE_CALLS.lock().unwrap().clone(),
        Some(std::env::temp_dir().display().to_string())
    );
}

#[test]
fn trace_symbol_missing_leaves_the_environment_in_charge() {
    let _guard = TRACE_LOCK.lock().unwrap();
    *TRACE_CALLS.lock().unwrap() = None;

    assert_eq!(
        apply_trace_directory(None, UcWidth::Utf16),
        TraceTarget::Environment
    );
    assert_eq!(*TRACE_CALLS.lock().unwrap(), None);
}

#[test]
fn trace_target_labels_are_stable_doctor_text() {
    assert_eq!(
        TraceTarget::Symbol.label(),
        "RFC trace directory: RfcSetTraceDir exported, set"
    );
    assert_eq!(
        TraceTarget::Environment.label(),
        "RFC trace directory: RfcSetTraceDir not exported, environment only"
    );
    assert_eq!(
        TraceTarget::Rejected(20).label(),
        "RFC trace directory: RfcSetTraceDir exported, returned rc 20, environment only"
    );
}

#[test]
fn trace_symbol_returned_error_is_reported_with_its_code() {
    let _guard = TRACE_LOCK.lock().unwrap();
    *TRACE_CALLS.lock().unwrap() = None;

    assert_eq!(
        apply_trace_directory(Some(fail_trace_dir), UcWidth::Utf16),
        TraceTarget::Rejected(RFC_TRACE_FAILURE)
    );
    // The call was made; only its result differs from the success case.
    assert_eq!(
        TRACE_CALLS.lock().unwrap().clone(),
        Some(std::env::temp_dir().display().to_string())
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
        vec![rfc, cm].into_iter().chain(sdk).collect::<Vec<_>>()
    );
}

/// The two GUI libraries of one Linux install, in discovery order.
fn linux_install_libraries(install: &Path) -> Vec<PathBuf> {
    vec![
        install.join("bin").join("unicode").join("libsapnwrfc.so"),
        install.join("bin").join("libsapnwrfccm.so"),
    ]
}

/// The Linux GUI candidates `Env::discover` yields before the SDK family.
fn linux_gui_candidates(files: &[PathBuf], directories: &[PathBuf]) -> Vec<PathBuf> {
    let fs = FakeFs::from_paths(files, directories);
    let discovered = Env::discover(Platform::Linux, &env_with(&[("HOME", "/home/DEV")]), &fs);
    let candidates = RfcLibrary::candidate_paths(&discovered);
    let sdk = sdk_libraries(
        Platform::Linux,
        &[
            Path::new("/home/DEV/sap/nwrfcsdk"),
            Path::new("/opt/sap/nwrfcsdk"),
            Path::new("/usr/local/sap/nwrfcsdk"),
        ],
    );

    assert!(candidates.ends_with(&sdk), "{candidates:?}");

    candidates[..candidates.len() - sdk.len()].to_vec()
}

#[test]
fn install_version_reads_every_number_in_the_name() {
    let cases = [
        ("8.10rev14", vec![8, 10, 14]),
        ("8.00rev5", vec![8, 0, 5]),
        ("8.10", vec![8, 10]),
        ("", Vec::new()),
    ];

    for (name, expected) in cases {
        assert_eq!(
            install_version(name),
            expected,
            "unexpected version for {name}"
        );
    }
}

#[test]
fn linux_versioned_install_folder_is_found() {
    let install = Path::new("/home/DEV/sapgui/SAPGUI8.10rev14");
    let libraries = linux_install_libraries(install);
    let directories = [
        PathBuf::from("/home/DEV/sapgui/SAPGUI"),
        install.to_path_buf(),
    ];

    assert_eq!(linux_gui_candidates(&libraries, &directories), libraries);
}

#[test]
fn linux_versioned_installs_are_ordered_highest_first() {
    let root = Path::new("/home/DEV/sapgui");
    let ranked = [
        root.join("SAPGUI8.00rev5"),
        root.join("SAPGUI8.10rev9"),
        root.join("SAPGUI8.10rev14"),
        root.join("SAPGUI10.0rev1"),
    ];
    let files = ranked
        .iter()
        .flat_map(|install| linux_install_libraries(install))
        .collect::<Vec<_>>();
    let expected = ranked
        .iter()
        .rev()
        .flat_map(|install| linux_install_libraries(install))
        .collect::<Vec<_>>();

    // Numeric order: 10.0 outranks 8.10, and revision 14 outranks revision 9.
    assert_eq!(linux_gui_candidates(&files, &ranked), expected);
}

#[test]
fn linux_launcher_only_gui_folder_is_ignored() {
    let root = Path::new("/home/DEV/sapgui");
    let launcher = root.join("SAPGUI").join("bin").join("guistart");
    let install = root.join("SAPGUI8.10rev14");
    let libraries = linux_install_libraries(&install);
    let files = [vec![launcher], libraries.clone()].concat();
    let directories = [root.join("SAPGUI"), install];

    assert_eq!(linux_gui_candidates(&files, &directories), libraries);
    assert!(linux_gui_candidates(&files[..1], &directories).is_empty());
}

#[test]
fn linux_direct_layout_still_works_beside_versioned_installs() {
    let root = Path::new("/opt/sapgui");
    let install = root.join("SAPGUI8.10rev14");
    let direct = linux_install_libraries(root);
    let versioned = linux_install_libraries(&install);
    let files = [direct.clone(), versioned.clone()].concat();

    assert_eq!(
        linux_gui_candidates(&files, &[install]),
        [direct, versioned].concat()
    );
}

#[test]
fn linux_roots_keep_their_order_across_versioned_installs() {
    let home_install = Path::new("/home/DEV/sapgui/SAPGUI8.00rev5");
    let opt_install = Path::new("/opt/sapgui/SAPGUI8.10rev14");
    let local_install = Path::new("/usr/local/sapgui/SAPGUI10.0rev1");
    let installs = [home_install, opt_install, local_install];
    let files = installs
        .iter()
        .flat_map(|install| linux_install_libraries(install))
        .collect::<Vec<_>>();
    let directories = installs.map(Path::to_path_buf);

    // Root order wins over version: the home install is tried first even though it is the oldest.
    assert_eq!(linux_gui_candidates(&files, &directories), files);
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
fn discovery_against_the_real_file_system_finds_an_installed_library() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let gui_root = home.join("sapgui");
    let installed = gui_root.join("bin").join("libsapnwrfccm.so");

    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, elf(2, 0x00b7)).unwrap();

    let env = env_with(&[("HOME", home.to_str().unwrap())]);
    let discovered = Env::discover(Platform::Linux, &env, &SystemFs);
    let candidates = RfcLibrary::candidate_paths(&discovered);

    // Only the SAP GUI library that exists becomes a GUI candidate, so `is_file` is not a
    // constant; the SDK paths stay in the list whether or not the files exist.
    assert_eq!(candidates[0], installed);
    assert!(
        !candidates.contains(&gui_root.join("bin").join("unicode").join("libsapnwrfc.so")),
        "{candidates:?}"
    );

    std::fs::remove_file(&installed).unwrap();

    let removed = Env::discover(Platform::Linux, &env, &SystemFs);

    assert!(
        !RfcLibrary::candidate_paths(&removed).contains(&installed),
        "a removed library is no longer a candidate"
    );
}

#[test]
fn system_file_system_lists_child_directories_sorted_and_skips_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();

    for name in ["bundle-b", "bundle-a"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }

    std::fs::write(root.join("bundle-c"), b"not a directory").unwrap();

    let children = SystemFs.child_directories(root);

    assert_eq!(children, [root.join("bundle-a"), root.join("bundle-b")]);
    assert!(SystemFs.child_directories(&root.join("absent")).is_empty());
    assert!(SystemFs.is_file(&root.join("bundle-c")));
    assert!(!SystemFs.is_file(root));
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

// --- Task 3a: architecture-aware, fail-closed loader decisions ---

/// The architecture declarations of one header, as the loader reads them.
fn archs(header: &[u8]) -> Result<Vec<Arch>, String> {
    declared_archs(header)
}

/// The PE image of a 16 KiB file: `e_magic`, `e_lfanew`, the PE signature and a machine word.
fn pe(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 16 * 1024];

    bytes[0..2].copy_from_slice(b"MZ");
    bytes[0x3c..PE_OFFSET_END].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    bytes[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
    bytes
}

/// The ELF image of a 16 KiB file: `e_ident`, `EI_CLASS`, `EI_DATA` and `e_machine`.
fn elf(class: u8, machine: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 16 * 1024];

    bytes[0..4].copy_from_slice(&ELF_MAGIC);
    bytes[4] = class;
    bytes[5] = 1;
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes
}

/// The thin 64-bit little-endian Mach-O image of a file: `MH_MAGIC_64` as stored, and `cputype`.
fn macho(cpu_type: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; 16 * 1024];

    bytes[0..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    bytes[4..8].copy_from_slice(&cpu_type.to_le_bytes());
    bytes
}

/// The thin 64-bit big-endian Mach-O image of a file: `MH_MAGIC_64` as stored, and `cputype`.
fn macho_big_endian(cpu_type: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; 16 * 1024];

    bytes[0..4].copy_from_slice(&[0xfe, 0xed, 0xfa, 0xcf]);
    bytes[4..8].copy_from_slice(&cpu_type.to_be_bytes());
    bytes
}

/// A universal Mach-O image whose slices are the given cpu types.
fn macho_universal(cpu_types: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0u8; 16 * 1024];

    bytes[0..4].copy_from_slice(&0xcafe_babeu32.to_be_bytes());
    bytes[4..8].copy_from_slice(&(cpu_types.len() as u32).to_be_bytes());

    for (index, cpu_type) in cpu_types.iter().enumerate() {
        let start = 8 + index * 20;

        bytes[start..start + 4].copy_from_slice(&cpu_type.to_be_bytes());
    }

    bytes
}

/// The candidates of one `Env`, in load order, without touching the file system.
fn candidates(env: &Env) -> Vec<Candidate> {
    RfcLibrary::candidates(env)
}

/// A candidate set built directly from `(path, origin)` pairs.
fn oracle(pairs: &[(&str, Origin)]) -> Vec<Candidate> {
    pairs
        .iter()
        .map(|(path, origin)| Candidate::new(PathBuf::from(path), *origin))
        .collect()
}

fn seen(checked: &mut Vec<String>, path: &Path) -> Result<(), String> {
    checked.push(path.display().to_string());
    Ok(())
}

fn refused(checked: &mut Vec<String>, path: &Path, reason: &str) -> Result<(), String> {
    seen(checked, path)?;
    Err(reason.to_owned())
}

/// Writes `header` as a candidate library and returns its path.
fn written_library(directory: &Path, name: &str, header: &[u8]) -> PathBuf {
    let path = directory.join(name);

    std::fs::write(&path, header).unwrap();
    path
}

/// The header check the loader runs before it maps a candidate: read the image, compare the
/// architecture. The mapping itself is replaced by `Ok`, so no library is ever loaded here.
fn header_check(expected: Arch) -> impl Fn(&Candidate) -> Result<(), String> {
    move |candidate| {
        let header = std::fs::read(&candidate.path).map_err(|error| error.to_string())?;

        compatible_arch(expected, &header)
    }
}

#[test]
fn pe_header_parsing_names_x86_x64_and_arm64() {
    assert_eq!(archs(&pe(0x014c)), Ok(vec![Arch::X86]));
    assert_eq!(archs(&pe(0x01c4)), Ok(vec![Arch::Arm32]));
    assert_eq!(archs(&pe(0x8664)), Ok(vec![Arch::X64]));
    assert_eq!(archs(&pe(0xaa64)), Ok(vec![Arch::Arm64]));
}

#[test]
fn elf_header_parsing_names_x86_64_and_aarch64() {
    assert_eq!(archs(&elf(1, 0x0003)), Ok(vec![Arch::X86]));
    assert_eq!(archs(&elf(1, 0x0028)), Ok(vec![Arch::Arm32]));
    assert_eq!(archs(&elf(2, 0x003e)), Ok(vec![Arch::X64]));
    assert_eq!(archs(&elf(2, 0x00b7)), Ok(vec![Arch::Arm64]));
}

#[test]
fn macho_header_parsing_names_every_slice() {
    assert_eq!(archs(&macho(0x0100_0007)), Ok(vec![Arch::X64]));
    assert_eq!(archs(&macho(0x0100_000c)), Ok(vec![Arch::Arm64]));
    assert_eq!(archs(&macho(0x0000_0007)), Ok(vec![Arch::X86]));
    assert_eq!(archs(&macho(0x0000_000c)), Ok(vec![Arch::Arm32]));

    // A 32-bit image stores the other magic pair, in the same byte order as its cpu type.
    assert_eq!(archs(&macho_big_endian(0x0000_0007)), Ok(vec![Arch::X86]));
    assert_eq!(
        archs(&{
            let mut image = macho(0x0000_000c);

            image[0..4].copy_from_slice(&[0xce, 0xfa, 0xed, 0xfe]);
            image
        }),
        Ok(vec![Arch::Arm32])
    );

    // A universal image declares every slice, so x86-64 and arm64 can share one file.
    assert_eq!(
        archs(&macho_universal(&[0x0100_0007, 0x0100_000c])),
        Ok(vec![Arch::X64, Arch::Arm64])
    );
    // A thin image may store its magic in either byte order; both declare the same cpu type.
    assert_eq!(archs(&macho_big_endian(0x0100_0007)), Ok(vec![Arch::X64]));
    assert_eq!(archs(&macho_big_endian(0x0100_000c)), Ok(vec![Arch::Arm64]));
    assert!(
        archs(&macho(0x0100_0012)).is_err(),
        "an unknown cpu type is not an architecture"
    );
    assert!(
        archs(&macho_universal(&[])).is_err(),
        "a universal image needs slices"
    );
    assert!(
        archs(&macho_universal(&[0x0100_0007; 40])).is_err(),
        "an implausible slice count is malformed"
    );
    // The slice limit itself is accepted, one more slice is not.
    assert_eq!(
        archs(&macho_universal(&[0x0100_0007; 32])),
        Ok(vec![Arch::X64; 32])
    );
    assert!(archs(&macho_universal(&[0x0100_0007; 33])).is_err());
}

#[test]
fn malformed_headers_are_rejected_with_a_reason() {
    let cases = [
        Vec::new(),
        b"MZ".to_vec(),
        b"\x7fELF".to_vec(),
        vec![0xff; 64],
        elf(3, 0x003e),
        elf(2, 0x1234),
        pe(0x1234),
    ];

    for header in cases {
        let error = archs(&header).expect_err("a malformed header is never an architecture");

        assert!(!error.is_empty(), "the rejection must carry a reason");
    }

    // A header that is not an image at all never reaches a machine word.
    assert!(archs(b"not a library image at all").is_err());
    // A PE header whose offset points outside the inspected region is malformed, not x86.
    let mut outside = vec![0u8; 0x40];

    outside[0..2].copy_from_slice(b"MZ");
    outside[0x3c..PE_OFFSET_END].copy_from_slice(&0x20_0000u32.to_le_bytes());
    assert!(archs(&outside).is_err());
    // A PE header without the signature is malformed even when the offset is in range.
    let mut unsigned = vec![0u8; 0x100];

    unsigned[0..2].copy_from_slice(b"MZ");
    unsigned[0x3c..PE_OFFSET_END].copy_from_slice(&0x80u32.to_le_bytes());
    assert!(archs(&unsigned).is_err());
    // An ELF header in big-endian encoding is not the image this loader reads.
    let mut big_endian = elf(2, 0x00b7);

    big_endian[5] = 2;
    assert!(archs(&big_endian).is_err());
    // A universal Mach-O header whose slice table is cut off is malformed.
    let mut truncated = macho_universal(&[0x0100_0007, 0x0100_000c]);

    truncated.truncate(12);
    assert!(archs(&truncated).is_err());

    let mut header_only = macho_universal(&[0x0100_0007]);

    header_only.truncate(6);
    assert!(archs(&header_only).is_err());
    assert!(archs(&[0xcf, 0xfa, 0xed]).is_err());
}

#[test]
fn a_rejected_architecture_names_both_sides() {
    let error =
        compatible_arch(Arch::Arm64, &elf(2, 0x003e)).expect_err("x86-64 cannot load into arm64");

    assert!(error.contains(Arch::X64.label()), "{error}");
    assert!(error.contains(Arch::Arm64.label()), "{error}");
    assert!(error.contains("mismatch"), "{error}");

    let error =
        compatible_arch(Arch::X64, &pe(0x014c)).expect_err("a 32-bit DLL cannot load into x64");

    assert!(error.contains(Arch::X86.label()), "{error}");
    assert!(error.contains(Arch::X64.label()), "{error}");
    // A universal image that carries the running architecture is accepted.
    assert_eq!(
        compatible_arch(Arch::Arm64, &macho_universal(&[0x0100_0007, 0x0100_000c])),
        Ok(())
    );
    assert_eq!(compatible_arch(Arch::Arm64, &elf(2, 0x00b7)), Ok(()));
    assert!(compatible_arch(Arch::Arm64, &macho(0x0100_0007)).is_err());
}

#[test]
fn the_running_architecture_matches_the_build_target() {
    let expected = if cfg!(target_arch = "x86_64") {
        Arch::X64
    } else if cfg!(target_arch = "aarch64") {
        Arch::Arm64
    } else if cfg!(target_arch = "x86") {
        Arch::X86
    } else {
        Arch::Arm32
    };

    assert_eq!(Arch::current(), Some(expected));
    assert_eq!(Arch::from_target(std::env::consts::ARCH), Arch::current());
    assert_eq!(Arch::from_target("x86"), Some(Arch::X86));
    assert_eq!(Arch::from_target("x86_64"), Some(Arch::X64));
    assert_eq!(Arch::from_target("arm"), Some(Arch::Arm32));
    assert_eq!(Arch::from_target("aarch64"), Some(Arch::Arm64));
    // A host architecture this build does not know is not silently treated as 32-bit arm.
    assert_eq!(Arch::from_target("powerpc64"), None);
    assert_eq!(Arch::from_target(""), None);
    assert_eq!(Arch::from_target("X86_64"), None);
    assert!(!expected.label().is_empty());
}

#[test]
fn the_override_is_explicit_and_discovery_is_not() {
    let env = Env::test_paths("/override", "/gui", "/sdk");
    // The location that found a discovered path supplies its family; the override is labelled by
    // the library the named path itself describes.
    let expected = [
        ("/override", Family::Sdk, Origin::Explicit),
        ("/gui", Family::Gui, Origin::Discovered),
        ("/sdk", Family::Sdk, Origin::Discovered),
    ];

    assert_eq!(
        candidates(&env),
        expected
            .iter()
            .map(|(path, family, origin)| Candidate::located(PathBuf::from(path), *family, *origin))
            .collect::<Vec<_>>()
    );
}

#[test]
fn automatic_candidate_fallback_skips_a_candidate_that_cannot_be_used() {
    let env = Env::test_paths("/override", "/gui", "/sdk");
    let discovered = &candidates(&env)[1..];
    let mut checked = Vec::new();
    let (index, ()) = decide(discovered, |candidate| {
        if candidate.path == Path::new("/gui") {
            refused(&mut checked, &candidate.path, "cannot be mapped")
        } else {
            seen(&mut checked, &candidate.path)
        }
    })
    .unwrap();

    assert_eq!(checked, ["/gui", "/sdk"]);
    assert_eq!(index, 1, "the SDK candidate is the one that was accepted");
    assert_eq!(discovered[index].path, PathBuf::from("/sdk"));
}

#[test]
fn wrong_architecture_candidate_is_reported_then_skipped() {
    let directory = tempfile::tempdir().unwrap();
    // For an arm64 binary the GUI candidate is an x86-64 ELF and the SDK candidate an arm64 ELF.
    let gui = written_library(directory.path(), "libsapnwrfccm.so", &elf(2, 0x003e));
    let sdk = written_library(directory.path(), "libsapnwrfc.so", &elf(2, 0x00b7));
    let candidates = oracle(&[
        (gui.to_str().unwrap(), Origin::Discovered),
        (sdk.to_str().unwrap(), Origin::Discovered),
    ]);

    // The rejected candidate is decided from its header alone, so it never reaches FFI loading.
    assert_eq!(
        header_check(Arch::Arm64)(&candidates[0]),
        Err(format!(
            "architecture mismatch: the library is {} but this binary is {}",
            Arch::X64.label(),
            Arch::Arm64.label()
        ))
    );

    let (index, ()) = decide(&candidates, header_check(Arch::Arm64)).unwrap();

    assert_eq!(
        index, 1,
        "the wrong-architecture candidate is skipped for the next one"
    );
    assert_eq!(candidates[index].path, sdk);

    // When nothing else is usable the skipped candidate is reported with both architectures.
    let error =
        decide(&candidates[..1], header_check(Arch::Arm64)).expect_err("no candidate can be used");
    let message = error.to_string();

    assert!(message.contains(&gui.display().to_string()), "{message}");
    assert!(message.contains(Arch::X64.label()), "{message}");
    assert!(message.contains(Arch::Arm64.label()), "{message}");
}

#[test]
fn a_32_bit_dll_on_a_64_bit_binary_is_reported_with_both_architectures() {
    let directory = tempfile::tempdir().unwrap();
    let dll = written_library(directory.path(), "sapnwrfccm.dll", &pe(0x014c));
    let header = std::fs::read(&dll).unwrap();
    let error = compatible_arch(Arch::X64, &header).expect_err("a 32-bit DLL cannot be mapped");

    assert!(error.contains("x86 (32-bit)"), "{error}");
    assert!(error.contains("x86-64"), "{error}");
}

#[test]
fn automatic_discovery_reports_every_failure_when_no_candidate_is_usable() {
    let mut checked = Vec::new();
    let candidates = oracle(&[("/gui", Origin::Discovered), ("/sdk", Origin::Discovered)]);
    let error = decide(&candidates, |candidate| {
        refused(&mut checked, &candidate.path, "cannot be mapped")
    })
    .expect_err("no candidate can be used");

    assert_eq!(checked, ["/gui", "/sdk"]);
    let message = error.to_string();

    assert!(message.contains("/gui: cannot be mapped"), "{message}");
    assert!(message.contains("/sdk: cannot be mapped"), "{message}");
    assert!(!message.contains("explicit path"), "{message}");
}

#[test]
fn explicit_override_or_explicit_gui_sdk_path_failure_attempts_no_other_candidate() {
    let mut checked = Vec::new();
    let candidates = candidates(&Env::test_paths("/override", "/gui", "/sdk"));
    let error = decide(&candidates, |candidate| {
        refused(&mut checked, &candidate.path, "wrong architecture")
    })
    .expect_err("an explicit path fails closed");

    assert_eq!(checked, ["/override"]);
    assert_eq!(
        error,
        LoadError::Explicit {
            path: PathBuf::from("/override"),
            reason: "wrong architecture".to_owned(),
        }
    );

    let message = error.to_string();

    assert!(message.contains("/override"), "{message}");
    assert!(message.contains("wrong architecture"), "{message}");
    assert!(message.contains("does not fall back"), "{message}");
    assert!(!message.contains("/gui"), "{message}");
    assert!(!message.contains("/sdk"), "{message}");
}

#[test]
fn a_discovered_candidate_never_stops_the_load() {
    let mut checked = Vec::new();
    let error = decide(&oracle(&[("/gui", Origin::Discovered)]), |candidate| {
        refused(&mut checked, &candidate.path, "cannot be mapped")
    })
    .expect_err("the only discovered candidate failed");

    assert_eq!(checked, ["/gui"]);
    assert!(matches!(error, LoadError::Exhausted(_)), "{error:?}");
}

#[test]
fn an_empty_candidate_list_reports_an_exhausted_load() {
    let error = decide(&[], |_| Ok(())).expect_err("there is no candidate to accept");

    assert_eq!(error, LoadError::Exhausted(Vec::new()));
}

#[test]
fn the_first_usable_candidate_wins() {
    let mut checked = Vec::new();
    let (index, ()) = decide(
        &oracle(&[("/gui", Origin::Discovered), ("/sdk", Origin::Discovered)]),
        |candidate| {
            if candidate.path == Path::new("/gui") {
                refused(&mut checked, &candidate.path, "wrong architecture")
            } else {
                seen(&mut checked, &candidate.path)
            }
        },
    )
    .unwrap();

    assert_eq!(index, 1);
    assert_eq!(checked, ["/gui", "/sdk"]);
}

#[test]
fn load_compatible_rejects_a_foreign_architecture_before_loading_it() {
    let directory = tempfile::tempdir().unwrap();
    let foreign = directory.path().join("libsapnwrfc.so");
    let header = match Arch::current().unwrap() {
        Arch::X64 | Arch::Arm64 => elf(2, 0x1234),
        Arch::X86 | Arch::Arm32 => pe(0x1234),
    };

    std::fs::write(&foreign, header).unwrap();

    // `Loaded` deliberately has no `Debug`, so the error is the value the test inspects.
    let error = load_compatible(&foreign)
        .map(|_| ())
        .expect_err("an unreadable header is never mapped");

    assert!(error.contains("unsupported"), "{error}");
    assert!(load_compatible(&directory.path().join("absent.so")).is_err());
}

#[test]
fn an_explicit_foreign_architecture_override_never_reaches_the_next_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let overridden = written_library(directory.path(), "libsapnwrfc.so", &elf(2, 0x003e));
    let installed = written_library(directory.path(), "libsapnwrfc.so.arm64", &elf(2, 0x00b7));
    let candidates = oracle(&[
        (overridden.to_str().unwrap(), Origin::Explicit),
        (installed.to_str().unwrap(), Origin::Discovered),
    ]);
    let mut checked = Vec::new();
    let error = decide(&candidates, |candidate| {
        checked.push(candidate.path.clone());
        header_check(Arch::Arm64)(candidate)
    })
    .expect_err("an explicit path fails closed");
    let message = error.to_string();

    // Only the named path was inspected, and the message states both sides.
    assert_eq!(checked, vec![overridden.clone()]);
    assert!(
        message.contains(&overridden.display().to_string()),
        "{message}"
    );
    assert!(message.contains(Arch::X64.label()), "{message}");
    assert!(message.contains(Arch::Arm64.label()), "{message}");
    assert!(message.contains("does not fall back"), "{message}");
    assert!(
        !message.contains(&installed.display().to_string()),
        "{message}"
    );
}

#[test]
fn a_missing_explicit_override_never_reaches_the_next_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let absent = directory.path().join("absent.so");
    let installed = written_library(directory.path(), "libsapnwrfc.so", &elf(2, 0x00b7));
    let candidates = oracle(&[
        (absent.to_str().unwrap(), Origin::Explicit),
        (installed.to_str().unwrap(), Origin::Discovered),
    ]);
    let mut checked = Vec::new();
    let error = decide(&candidates, |candidate| {
        checked.push(candidate.path.clone());
        header_check(Arch::Arm64)(candidate)
    })
    .expect_err("a missing explicit path fails closed");
    let message = error.to_string();

    assert_eq!(checked, vec![absent.clone()]);
    assert!(message.contains("does not fall back"), "{message}");
    assert!(
        !message.contains(&installed.display().to_string()),
        "{message}"
    );
}

// --- Task 3b: OS loader adapters and library identity ---

/// An ELF64 image whose dynamic section holds `DT_STRTAB`, `DT_STRSZ`, then `DT_NULL`, and one
/// `DT_RUNPATH` past the terminator. A reader that ignores the terminator would incorrectly see
/// an origin path.
fn elf_with_a_run_path_past_the_terminator(past: &str) -> Vec<u8> {
    let mut strings = vec![0u8];
    let mut add = |name: &str| {
        let offset = strings.len() as u64;

        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        offset
    };
    let past_offset = add(past);
    let entries = [
        (5i64, ELF_STRINGS as u64),
        (10i64, strings.len() as u64),
        (0i64, 0),
        (29i64, past_offset),
    ];
    let mut image = vec![0u8; ELF_STRINGS + strings.len()];
    let (class, machine) = host_elf_machine();
    let mapped = image.len() as u64;

    image[0..4].copy_from_slice(&ELF_MAGIC);
    image[4] = class;
    image[5] = 1;
    image[18..20].copy_from_slice(&machine.to_le_bytes());
    image[0x20..0x28].copy_from_slice(&(ELF_PHDRS as u64).to_le_bytes());
    image[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
    image[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());
    image[ELF_PHDRS..ELF_PHDRS + 4].copy_from_slice(&1u32.to_le_bytes());
    image[ELF_PHDRS + 32..ELF_PHDRS + 40].copy_from_slice(&mapped.to_le_bytes());
    image[ELF_PHDRS + 56..ELF_PHDRS + 60].copy_from_slice(&2u32.to_le_bytes());
    image[ELF_PHDRS + 64..ELF_PHDRS + 72].copy_from_slice(&(ELF_DYNAMIC as u64).to_le_bytes());
    image[ELF_PHDRS + 88..ELF_PHDRS + 96]
        .copy_from_slice(&(entries.len() as u64 * 16).to_le_bytes());

    for (index, (tag, value)) in entries.iter().enumerate() {
        let start = ELF_DYNAMIC + index * 16;

        image[start..start + 8].copy_from_slice(&tag.to_le_bytes());
        image[start + 8..start + 16].copy_from_slice(&value.to_le_bytes());
    }

    image[ELF_STRINGS..].copy_from_slice(&strings);
    image
}

/// Where the ELF64 fixture puts its parts: two program headers after the header, then up to eight
/// dynamic entries, then the string table.
const ELF_PHDRS: usize = 0x40;
const ELF_DYNAMIC: usize = ELF_PHDRS + 2 * 56;
const ELF_STRINGS: usize = ELF_DYNAMIC + 8 * 16;

/// The ELF64 image of a shared library with a dynamic section: `needed` dependency names and the
/// `run_path` or `rpath` entry, each when one is given.
fn elf_with_dynamic(needed: &[&str], run_path: Option<&str>, rpath: Option<&str>) -> Vec<u8> {
    const PHDRS: usize = ELF_PHDRS;
    const DYNAMIC: usize = ELF_DYNAMIC;
    const STRINGS: usize = ELF_STRINGS;
    let mut strings = vec![0u8];
    let mut add = |name: &str| {
        let offset = strings.len() as u64;

        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        offset
    };
    let needed_offsets = needed.iter().map(|name| add(name)).collect::<Vec<_>>();
    let run_path_offset = run_path.map(&mut add);
    let rpath_offset = rpath.map(&mut add);
    let mut entries = needed_offsets
        .into_iter()
        .map(|offset| (1i64, offset))
        .collect::<Vec<_>>();

    entries.extend(run_path_offset.map(|offset| (29, offset)));
    entries.extend(rpath_offset.map(|offset| (15, offset)));
    entries.push((5, STRINGS as u64));
    entries.push((10, strings.len() as u64));
    entries.push((0, 0));

    let image_bytes = STRINGS + strings.len();
    let mut image = vec![0u8; image_bytes];

    let (class, machine) = host_elf_machine();

    image[0..4].copy_from_slice(&ELF_MAGIC);
    image[4] = class;
    image[5] = 1;
    image[18..20].copy_from_slice(&machine.to_le_bytes());
    image[0x20..0x28].copy_from_slice(&(PHDRS as u64).to_le_bytes());
    image[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
    image[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());

    // One load segment maps every file offset to the same virtual address, so every value in the
    // dynamic section that is a virtual address is also the file offset; the second program
    // header is the dynamic one.
    image[PHDRS..PHDRS + 4].copy_from_slice(&1u32.to_le_bytes());
    image[PHDRS + 32..PHDRS + 40].copy_from_slice(&(image_bytes as u64).to_le_bytes());
    image[PHDRS + 56..PHDRS + 60].copy_from_slice(&2u32.to_le_bytes());
    image[PHDRS + 64..PHDRS + 72].copy_from_slice(&(DYNAMIC as u64).to_le_bytes());
    image[PHDRS + 88..PHDRS + 96].copy_from_slice(&(entries.len() as u64 * 16).to_le_bytes());

    for (index, (tag, value)) in entries.iter().enumerate() {
        let start = DYNAMIC + index * 16;

        image[start..start + 8].copy_from_slice(&tag.to_le_bytes());
        image[start + 8..start + 16].copy_from_slice(&value.to_le_bytes());
    }

    image[STRINGS..].copy_from_slice(&strings);
    image
}

/// The ELF `EI_CLASS` and `e_machine` of the machine running the tests, so a fixture image passes
/// the architecture check and reaches the platform step the test is about.
fn host_elf_machine() -> (u8, u16) {
    match std::env::consts::ARCH {
        "x86_64" => (2, 0x003e),
        "aarch64" => (2, 0x00b7),
        "x86" => (1, 0x0003),
        _ => (1, 0x0028),
    }
}

fn dynamic(image: &[u8]) -> Result<ElfDynamic, String> {
    elf_dynamic(std::io::Cursor::new(image))
}

/// An SDK candidate for the platform-gated adapter tests.
#[cfg(any(windows, target_os = "linux"))]
fn discovered_sdk(path: &Path) -> Candidate {
    Candidate {
        path: path.to_path_buf(),
        family: Family::Sdk,
        origin: Origin::Discovered,
    }
}

#[test]
fn library_identity_reports_path_version_architecture_family_and_source() {
    let identity = LibraryIdentity::new(
        PathBuf::from("/opt/sap/nwrfcsdk/lib/libsapnwrfc.so"),
        "7.53.1629".to_owned(),
        Arch::Arm64,
        Family::Sdk,
        Origin::Discovered,
        UcWidth::Utf16,
    );

    assert_eq!(
        identity.path(),
        Path::new("/opt/sap/nwrfcsdk/lib/libsapnwrfc.so")
    );
    assert_eq!(identity.version(), "7.53.1629");
    assert_eq!(identity.arch(), Arch::Arm64);
    assert_eq!(identity.family(), Family::Sdk);
    assert_eq!(identity.source(), Origin::Discovered);
    assert_eq!(
        identity.report(),
        concat!(
            "/opt/sap/nwrfcsdk/lib/libsapnwrfc.so version 7.53.1629 architecture arm64 ",
            "family NW RFC SDK source discovered chars UTF-16"
        )
    );

    let gui = LibraryIdentity::new(
        PathBuf::from("/opt/sapgui/bin/libsapnwrfccm.so"),
        "7.50.17".to_owned(),
        Arch::X64,
        Family::Gui,
        Origin::Explicit,
        UcWidth::Narrow,
    );

    assert_eq!(
        gui.report(),
        concat!(
            "/opt/sapgui/bin/libsapnwrfccm.so version 7.50.17 architecture x86-64 ",
            "family SAP GUI source explicit ABAPREACH_RFC_LIBRARY chars single-byte"
        )
    );
}

#[test]
fn a_library_family_and_source_each_render_one_documented_label() {
    assert_eq!(Family::Gui.label(), "SAP GUI");
    assert_eq!(Family::Sdk.label(), "NW RFC SDK");
    assert_eq!(Origin::Explicit.label(), "explicit ABAPREACH_RFC_LIBRARY");
    assert_eq!(Origin::Discovered.label(), "discovered");
}

#[test]
fn family_of_names_a_gui_package_and_an_sdk_folder() {
    let cases = [
        (
            r"C:\Program Files\SAP\FrontEnd\SAPGUI\sapnwrfccm.dll",
            Family::Gui,
        ),
        (
            concat!(
                "/Applications/SAP Clients/SAPGUI 8.10rev15/SAPGUI 8.10rev15.app",
                "/Contents/Resources/bin/unicode/libsapnwrfc.dylib"
            ),
            Family::Gui,
        ),
        ("/opt/sapgui/bin/libsapnwrfccm.so", Family::Gui),
        ("/opt/sapgui/bin/unicode/libsapnwrfc.so", Family::Gui),
        ("/usr/local/sapgui/bin/libsapnwrfccm.so", Family::Gui),
        ("/custom/libraries/libsapnwrfccm.dylib", Family::Gui),
        ("/opt/sap/nwrfcsdk/lib/libsapnwrfc.so", Family::Sdk),
        ("/home/DEV/sap/nwrfcsdk/lib/libsapnwrfc.dylib", Family::Sdk),
        (r"C:\SAPNWRFC\lib\sapnwrfc.dll", Family::Sdk),
    ];

    for (path, family) in cases {
        assert_eq!(family_of(Path::new(path)), family, "{path}");
    }
}

#[test]
fn a_candidate_keeps_the_family_and_source_it_was_discovered_with() {
    let root = Path::new("/opt/sapgui");
    let cm = root.join("bin").join("libsapnwrfccm.so");
    let rfc = root.join("bin").join("unicode").join("libsapnwrfc.so");
    let fs = FakeFs::new(&[&cm, &rfc], &[]);
    let found = candidates(&Env::discover(
        Platform::Linux,
        &env_with(&[("HOME", "/home/DEV")]),
        &fs,
    ));

    assert_eq!(found[0].path, rfc);
    assert_eq!(found[0].family, Family::Gui);
    assert_eq!(found[0].origin, Origin::Discovered);
    assert_eq!(found[1].path, cm);
    assert_eq!(found[1].family, Family::Gui);
    assert!(found[2..].iter().all(|c| c.family == Family::Sdk));

    let overridden = candidates(&Env::discover(
        Platform::Linux,
        &env_with(&[("ABAPREACH_RFC_LIBRARY", "/opt/sapgui/bin/libsapnwrfccm.so")]),
        &FakeFs::new(&[], &[]),
    ));

    // An override is labelled by the library it names, not by its candidate position.
    assert_eq!(overridden[0].family, Family::Gui);
    assert_eq!(overridden[0].origin, Origin::Explicit);
    assert!(overridden[1..].iter().all(|c| c.family == Family::Sdk));
}

#[test]
fn windows_load_flags_use_the_altered_search_path_only_for_an_absolute_path() {
    // The flag makes `LoadLibraryExW` resolve a DLL's dependencies from that DLL's own folder,
    // and the API rejects it for a relative path.
    assert_eq!(LOAD_WITH_ALTERED_SEARCH_PATH, 0x0000_0008);
    assert_eq!(
        windows_load_flags(Path::new(
            r"C:\Program Files\SAP\FrontEnd\SAPGUI\sapnwrfccm.dll"
        )),
        LOAD_WITH_ALTERED_SEARCH_PATH
    );
    assert_eq!(windows_load_flags(Path::new("sapnwrfccm.dll")), 0);
}

#[test]
fn elf_dynamic_reads_only_the_run_path_needed_for_origin_detection() {
    let sdk = dynamic(&elf_with_dynamic(
        &["libsapucum.so", "libicuuc.so.57"],
        Some("/opt/rh/SAP/lib64:/usr/sap/lib"),
        None,
    ))
    .unwrap();

    assert_eq!(sdk.run_path(), ["/opt/rh/SAP/lib64:/usr/sap/lib"]);
    // The SDK family has no `$ORIGIN`, so its dependencies come from its own folder.
    assert!(!sdk.declares_origin());

    let gui = dynamic(&elf_with_dynamic(
        &["libsapucum.so"],
        Some("$ORIGIN/unicode:$ORIGIN/../lib"),
        None,
    ))
    .unwrap();

    assert!(gui.declares_origin());
    // `RPATH` is the older spelling of the same search path.
    assert!(
        dynamic(&elf_with_dynamic(&[], None, Some("$ORIGIN/lib")))
            .unwrap()
            .declares_origin()
    );
    // `$ORIGIN` counts only as a path component of its own, not as the start of another name.
    assert!(
        !dynamic(&elf_with_dynamic(&[], Some("$ORIGINATES/lib"), None))
            .unwrap()
            .declares_origin()
    );
    // An image with neither dependency metadata nor a search path declares neither.
    let empty = dynamic(&elf_with_dynamic(&[], None, None)).unwrap();

    assert!(empty.run_path().is_empty());
    assert!(!empty.declares_origin());
}

#[test]
fn an_unusable_elf_dynamic_section_is_reported() {
    let mut without_dynamic = elf_with_dynamic(&["libsapucum.so"], None, None);

    // No program header means no dynamic section, so nothing names a dependency.
    without_dynamic[0x38..0x3a].copy_from_slice(&0u16.to_le_bytes());

    let cases = [
        b"not an ELF image at all".to_vec(),
        elf_with_dynamic(&["libsapucum.so"], None, None)[..0x40].to_vec(),
        without_dynamic,
    ];

    for image in cases {
        let error = dynamic(&image).expect_err("an unusable dynamic section is never a decision");

        assert!(!error.is_empty(), "the rejection must carry a reason");
    }
}

#[test]
fn sdk_icu_set_preloads_the_highest_complete_real_sdk_version_in_dependency_order() {
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
    let library = directory.join("libsapnwrfc.so");
    let files = [
        "libicudata56.so",
        "libicuuc56.so",
        "libicui18n56.so",
        "libicudata57.so",
        "libicuuc57.so",
        "libicui18n57.so",
        "libsapucum.so",
    ]
    .into_iter()
    .map(|name| directory.join(name))
    .collect::<Vec<_>>();

    assert_eq!(
        icu_dependencies(&library, &files),
        Some(vec![
            directory.join("libicudata57.so"),
            directory.join("libicuuc57.so"),
            directory.join("libicui18n57.so"),
        ])
    );
}

#[test]
fn sdk_icu_preload_uses_a_complete_set_or_notes_that_ld_library_path_may_resolve_it() {
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
    let library = directory.join("libsapnwrfc.so");
    let no_origin = dynamic(&elf_with_dynamic(
        &[],
        Some("/opt/rh/SAP/lib64:/usr/sap/lib"),
        None,
    ))
    .unwrap();
    let files = vec![
        directory.join("libicudata57.so"),
        directory.join("libicuuc57.so"),
    ];

    let plan = dependency_preload(&library, Some(&no_origin), &files).unwrap();
    assert!(plan.paths.is_empty());
    assert_eq!(
        plan.note.as_deref(),
        Some(
            "WARNING: RFC ICU libraries were not found beside the RFC library; the NW RFC SDK may still find them through LD_LIBRARY_PATH."
        )
    );

    let origin = dynamic(&elf_with_dynamic(&[], Some("$ORIGIN"), None)).unwrap();
    assert!(
        dependency_preload(&library, Some(&origin), &files)
            .unwrap()
            .paths
            .is_empty()
    );
    assert!(
        dependency_preload(&library, None, &files)
            .expect_err("an unreadable dynamic section rejects the candidate before it is mapped")
            .contains("dependencies cannot be read")
    );
}

#[test]
fn preload_dependencies_maps_each_sibling_by_absolute_path_in_order() {
    let search_path = std::env::var_os("LD_LIBRARY_PATH");
    // Absolute on every OS: `/opt/...` has no drive, so Windows does not call it absolute.
    let directory = std::env::temp_dir().join("nwrfcsdk").join("lib");
    let dependencies = vec![
        directory.join("libsapucum.so"),
        directory.join("libicuuc.so.57"),
    ];
    let mut opened = Vec::new();
    let mapped = preload_dependencies(&dependencies, |path| {
        opened.push(path.to_path_buf());

        Ok(path.to_path_buf())
    })
    .unwrap();

    assert_eq!(mapped, dependencies);
    assert_eq!(opened, dependencies);
    assert!(opened.iter().all(|path| path.is_absolute()), "{opened:?}");
    // Preloading never rewrites the search path the parent process handed to this binary.
    assert_eq!(std::env::var_os("LD_LIBRARY_PATH"), search_path);

    let error = preload_dependencies(&dependencies, |path| {
        if path == dependencies[1] {
            Err("cannot be mapped".to_owned())
        } else {
            Ok(path.to_path_buf())
        }
    })
    .expect_err("a dependency that cannot be mapped stops the preload");

    assert!(
        error.contains(&dependencies[1].display().to_string()),
        "{error}"
    );
    assert!(error.contains("cannot be mapped"), "{error}");
    assert!(preload_dependencies(&[], |_| Ok(())).unwrap().is_empty());
}

#[cfg(windows)]
#[test]
fn windows_adapter_maps_an_absolute_dll_with_the_altered_search_path() {
    use abapreach::transport::rfc::load_candidate;

    let directory = tempfile::tempdir().unwrap();
    let library = built_shared_object(directory.path());

    // No SAP library is needed: the adapter maps a real PE through `LoadLibraryExW`.
    load_candidate(&discovered_sdk(&library))
        .map(|_| ())
        .expect("the adapter must map an absolute DLL");
    assert!(
        load_candidate(&discovered_sdk(&directory.path().join("absent.dll"))).is_err(),
        "an absent DLL cannot be mapped"
    );
}

/// An SDK fixture with the real 7.50 SDK ICU SONAMEs. The first ICU file is deliberately not a
/// shared object, so the preload fails before the RFC library itself can be mapped.
#[cfg(target_os = "linux")]
fn library_with_an_unloadable_icu_set() -> (PathBuf, PathBuf, String) {
    use abapreach::transport::rfc::load_candidate;

    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("libsapnwrfc.so");
    let icu_data = directory.path().join("libicudata57.so");
    let icu_uc = directory.path().join("libicuuc57.so");
    let icu_i18n = directory.path().join("libicui18n57.so");

    std::fs::write(
        &library,
        elf_with_dynamic(&[], Some("/opt/rh/SAP/lib64:/usr/sap/lib"), None),
    )
    .unwrap();
    std::fs::write(&icu_data, b"not a shared object").unwrap();
    std::fs::write(&icu_uc, b"not reached").unwrap();
    std::fs::write(&icu_i18n, b"not reached").unwrap();

    let error = load_candidate(&discovered_sdk(&library))
        .map(|_| ())
        .expect_err("the SDK family preloads ICU before it maps the library");

    // The temporary directory goes away with the tuple, so the assertion happens in the caller.
    (library, icu_data, error)
}

#[cfg(target_os = "linux")]
#[test]
fn linux_adapter_preloads_the_sdk_icu_set_by_absolute_path() {
    let (_, icu_data, error) = library_with_an_unloadable_icu_set();

    assert!(
        error.contains(&icu_data.display().to_string()),
        "the preload must name the absolute ICU path: {error}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_adapter_rejects_an_unreadable_dynamic_section_before_mapping() {
    use abapreach::transport::rfc::load_candidate;

    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("libsapnwrfc.so");
    let mut image = elf_with_dynamic(&["libsapucum.so"], None, None);
    image[0x38..0x3a].copy_from_slice(&0u16.to_le_bytes());
    std::fs::write(&library, image).unwrap();

    let error = load_candidate(&discovered_sdk(&library))
        .map(|_| ())
        .expect_err("a required dynamic read failure stops this candidate before dlopen");

    assert!(error.contains("dependencies cannot be read"), "{error}");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_adapter_preloads_with_an_empty_ld_library_path() {
    // `LD_LIBRARY_PATH` is read when a process starts, so only a child process can prove that the
    // same-directory preload works without it.
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "linux_adapter_child_preloads_with_an_empty_ld_library_path",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("ABAPREACH_LOADER_CHILD", "1")
        .env("LD_LIBRARY_PATH", "")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);

    assert!(
        child.status.success() && stdout.contains("1 passed"),
        "child failed: {stdout}{}",
        String::from_utf8_lossy(&child.stderr)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_adapter_child_preloads_with_an_empty_ld_library_path() {
    // A plain `cargo test` run reaches this test too; only the spawned child does the load.
    if std::env::var_os("ABAPREACH_LOADER_CHILD").is_none() {
        return;
    }

    assert_eq!(std::env::var("LD_LIBRARY_PATH").unwrap_or_default(), "");

    let (_, icu_data, error) = library_with_an_unloadable_icu_set();

    assert!(
        error.contains(&icu_data.display().to_string()),
        "the preload must not depend on the search path: {error}"
    );
    // The adapter reads the search path but never rewrites it for this process.
    assert_eq!(std::env::var("LD_LIBRARY_PATH").unwrap_or_default(), "");
}

/// A dynamic section is a sequence of entries, and `DT_NULL` ends it: a run path after the
/// terminator belongs to nothing, so it is never read.
#[test]
fn a_dynamic_section_stops_at_its_null_entry() {
    let image = elf_with_a_run_path_past_the_terminator("$ORIGIN");
    let entries = dynamic(&image).unwrap();

    assert!(entries.run_path().is_empty());
}

/// An ELF image whose class or data encoding the loader cannot read is refused, not guessed: only
/// 64-bit little-endian images are read.
#[test]
fn an_elf_image_of_another_class_or_byte_order_is_refused() {
    for (class, encoding) in [(1u8, 1u8), (2, 2), (0, 0)] {
        let mut image = elf_with_dynamic(&["libsapucum.so"], None, None);

        image[4] = class;
        image[5] = encoding;

        let error = dynamic(&image).expect_err("a foreign ELF class is not read");

        assert!(error.contains("unsupported ELF image"), "{error}");
    }
}

/// A dynamic section longer than the reader accepts is refused before any buffer is allocated.
#[test]
fn a_section_longer_than_the_read_limit_is_refused() {
    let mut image = elf_with_dynamic(&["libsapucum.so"], None, None);

    image[ELF_PHDRS + 88..ELF_PHDRS + 96].copy_from_slice(&(MAX_READ_BYTES + 1).to_le_bytes());

    let error = dynamic(&image).expect_err("an implausible section length is never read");

    assert!(error.contains("implausible section length"), "{error}");
}

/// A dynamic section of exactly the read limit is read; the limit itself is not the rejection.
#[test]
fn a_section_of_exactly_the_read_limit_is_read() {
    let mut image = elf_with_dynamic(&[], None, None);

    image.resize(ELF_DYNAMIC + MAX_READ_BYTES as usize, 0);
    image[ELF_PHDRS + 88..ELF_PHDRS + 96].copy_from_slice(&MAX_READ_BYTES.to_le_bytes());

    let entries = dynamic(&image).expect("a section of exactly the limit is readable");

    assert!(entries.run_path().is_empty());
}

/// A Windows full path: a drive-qualified path or a UNC share. A relative name, or a drive letter
/// that is not followed by a separator, is not a full path.
#[test]
fn a_windows_full_path_is_drive_qualified_or_a_unc_share() {
    let full = [
        r"C:\libraries\sapnwrfc.dll",
        r"c:/libraries/sapnwrfc.dll",
        r"\\\\host\\share\\sapnwrfc.dll",
        r"\\libraries\\sapnwrfc.dll",
    ];

    for path in full {
        assert_eq!(
            windows_load_flags(Path::new(path)),
            LOAD_WITH_ALTERED_SEARCH_PATH,
            "{path}"
        );
    }

    let relative = [
        "sapnwrfc.dll",
        "libraries/sapnwrfc.dll",
        "C:sapnwrfc.dll",
        "",
    ];

    for path in relative {
        assert_eq!(windows_load_flags(Path::new(path)), 0, "{path}");
    }
}

fn icu_files(directory: &Path, names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(|name| directory.join(name)).collect()
}

#[test]
fn icu_selection_ranks_versions_numerically_and_orders_data_uc_i18n() {
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
    let library = directory.join("libsapnwrfc.so");
    // Lexicographic order would pick `9`; the numeric rank picks `100`. Both spellings are read
    // and the files arrive out of dependency order.
    let files = icu_files(
        directory,
        &[
            "libicui18n.so.100",
            "libicui18n9.so",
            "libicuuc9.so",
            "libicudata9.so",
            "libicuuc.so.100",
            "libicui18n57.so",
            "libicudata.so.100",
            "libicuuc57.so",
            "libicudata57.so",
        ],
    );

    assert_eq!(
        icu_dependencies(&library, &files),
        Some(icu_files(
            directory,
            &["libicudata.so.100", "libicuuc.so.100", "libicui18n.so.100"]
        ))
    );
}

#[test]
fn icu_selection_compares_every_numeric_part_of_a_version() {
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
    let library = directory.join("libsapnwrfc.so");
    let set = |version: &str| {
        ["libicudata", "libicuuc", "libicui18n"].map(|prefix| format!("{prefix}{version}.so"))
    };
    let files = |versions: &[&str]| {
        versions
            .iter()
            .flat_map(|version| set(version))
            .map(|name| directory.join(name))
            .collect::<Vec<_>>()
    };
    let expected = |version: &str| Some(set(version).map(|name| directory.join(name)).to_vec());

    // `57.10` outranks `57.2`, and `57.2` outranks `57`.
    assert_eq!(
        icu_dependencies(&library, &files(&["57", "57.10", "57.2"])),
        expected("57.10")
    );
    // A version without digits ranks below any numbered one, wherever it sorts as text.
    assert_eq!(
        icu_dependencies(&library, &files(&["57", "abc"])),
        expected("57")
    );
    assert_eq!(
        icu_dependencies(&library, &files(&["abc"])),
        expected("abc")
    );
}

#[test]
fn icu_selection_skips_incomplete_sets_and_other_directories() {
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
    let library = directory.join("libsapnwrfc.so");
    let mut files = icu_files(
        directory,
        &[
            "libicudata60.so",
            "libicuuc60.so",
            "libicudata57.so",
            "libicuuc57.so",
            "libicui18n57.so",
            "libicui18n.so.59.dll",
            "libicuuc.so59",
            "libsapucum.so",
        ],
    );

    // Version 60 lacks its i18n file, so the complete 57 wins.
    assert_eq!(
        icu_dependencies(&library, &files),
        Some(icu_files(
            directory,
            &["libicudata57.so", "libicuuc57.so", "libicui18n57.so"]
        ))
    );

    // A complete set in another folder is not this library's.
    files.extend(icu_files(
        Path::new("/elsewhere"),
        &["libicudata70.so", "libicuuc70.so", "libicui18n70.so"],
    ));
    assert_eq!(
        icu_dependencies(&library, &files),
        Some(icu_files(
            directory,
            &["libicudata57.so", "libicuuc57.so", "libicui18n57.so"]
        ))
    );
    assert_eq!(icu_dependencies(&library, &files[..2]), None);
    assert_eq!(icu_dependencies(&library, &[]), None);
}

#[test]
fn icu_files_without_a_version_never_form_a_set() {
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
    let library = directory.join("libsapnwrfc.so");

    for names in [
        ["libicudata.so", "libicuuc.so", "libicui18n.so"],
        ["libicudata.so.", "libicuuc.so.", "libicui18n.so."],
    ] {
        assert_eq!(
            icu_dependencies(&library, &icu_files(directory, &names)),
            None,
            "{names:?}"
        );
    }
}

#[test]
fn one_icu_file_per_version_and_prefix_is_chosen_by_the_smallest_path() {
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
    let library = directory.join("libsapnwrfc.so");
    // Both spellings name version 57; `libicuuc.so.57` sorts before `libicuuc57.so`.
    let spellings = ["libicuuc57.so", "libicuuc.so.57"];
    let expected = Some(icu_files(
        directory,
        &["libicudata57.so", "libicuuc.so.57", "libicui18n57.so"],
    ));

    for names in [spellings, [spellings[1], spellings[0]]] {
        let mut files = icu_files(directory, &["libicudata57.so", "libicui18n57.so"]);

        files.extend(icu_files(directory, &names));
        assert_eq!(icu_dependencies(&library, &files), expected);
    }
}

/// A real, loadable shared object for this host: an empty `cdylib` built by the same toolchain
/// that builds the tests, so no SAP library is needed to exercise the whole load path.
fn built_shared_object(directory: &Path) -> PathBuf {
    let source = directory.join("empty.rs");
    // The extension matters: `LoadLibraryExW` appends `.dll` to a name that has none.
    let library = directory.join(format!(
        "libempty_fixture.{}",
        std::env::consts::DLL_EXTENSION
    ));

    std::fs::write(&source, "").unwrap();

    let status = std::process::Command::new(std::env::var_os("RUSTC").unwrap_or("rustc".into()))
        .args(["--crate-type", "cdylib", "--crate-name", "empty_fixture"])
        .arg("-o")
        .arg(&library)
        .arg(&source)
        .status()
        .expect("rustc must be on the PATH to build the shared object fixture");

    assert!(status.success(), "rustc could not build the fixture");

    library
}

const MISSING_ICU_WARNING: &str = "WARNING: RFC ICU libraries were not found beside the RFC library; the NW RFC SDK may still find them through LD_LIBRARY_PATH.";

#[test]
fn a_loaded_library_surfaces_the_loader_notes_of_its_platform() {
    let directory = tempfile::tempdir().unwrap();
    let library = built_shared_object(directory.path());
    let loaded = RfcLibrary::load(&Env::test_paths(library.to_str().unwrap(), "", ""))
        .unwrap_or_else(|error| panic!("the fixture must load: {error}"));

    // Only Linux preloads dependencies, and there the fixture has no ICU set beside it.
    if cfg!(target_os = "linux") {
        assert_eq!(loaded.notes(), [MISSING_ICU_WARNING]);
    } else {
        assert!(loaded.notes().is_empty(), "{:?}", loaded.notes());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_adapter_reports_no_note_when_the_icu_set_is_beside_the_library() {
    let directory = tempfile::tempdir().unwrap();
    let library = built_shared_object(directory.path());

    // The preloaded files only have to be mappable shared objects.
    for name in ["libicudata57.so", "libicuuc57.so", "libicui18n57.so"] {
        std::fs::copy(&library, directory.path().join(name)).unwrap();
    }

    let loaded = RfcLibrary::load(&Env::test_paths(library.to_str().unwrap(), "", ""))
        .unwrap_or_else(|error| panic!("the fixture must load: {error}"));

    assert!(loaded.notes().is_empty(), "{:?}", loaded.notes());
}

/// The first bytes `RfcGetVersion` returned on the machines this was measured on: the SAP GUI 8.00
/// for Windows library "753 Patch Level 1524" as single bytes, and an SDK as UTF-16.
const NARROW_VERSION_TEXT: [u8; 4] = [0x37, 0x35, 0x33, 0x20];
const UTF16_VERSION_TEXT: [u8; 4] = [0x37, 0x00, 0x35, 0x00];

#[test]
fn the_character_width_is_read_from_the_version_text() {
    assert_eq!(UcWidth::detect(&NARROW_VERSION_TEXT), UcWidth::Narrow);
    assert_eq!(UcWidth::detect(&UTF16_VERSION_TEXT), UcWidth::Utf16);
    // Nothing to tell apart reads as the width every other library has.
    assert_eq!(UcWidth::detect(&[]), UcWidth::Utf16);
    assert_eq!(UcWidth::detect(&[0x37]), UcWidth::Utf16);
    assert_eq!(UcWidth::detect(&[0, 0]), UcWidth::Utf16);
    assert_eq!(UcWidth::detect(&[0, 0x37]), UcWidth::Utf16);
}

fn bytes_of(text: &UcString, count: usize) -> Vec<u8> {
    // The string owns at least `count` bytes, and every byte of it is initialized.
    unsafe { std::slice::from_raw_parts(text.as_ptr().cast::<u8>(), count).to_vec() }
}

#[test]
fn a_string_is_encoded_in_the_width_of_the_library_that_reads_it() {
    let wide = UcString::encode(UcWidth::Utf16, "ASHOST").unwrap();
    let narrow = UcString::encode(UcWidth::Narrow, "ASHOST").unwrap();

    assert_eq!((wide.chars(), narrow.chars()), (6, 6));
    assert_eq!(
        bytes_of(&wide, 14),
        [b'A', 0, b'S', 0, b'H', 0, b'O', 0, b'S', 0, b'T', 0, 0, 0]
    );
    // Two terminators, so a read in the wrong width still ends inside the buffer.
    assert_eq!(bytes_of(&narrow, 8), b"ASHOST\0\0");
    assert_eq!(UcString::encode(UcWidth::Narrow, "").unwrap().chars(), 0);
}

#[test]
fn a_single_byte_library_refuses_text_it_cannot_hold() {
    let wide = UcString::encode(UcWidth::Utf16, "Aé").unwrap();
    let error = UcString::encode(UcWidth::Narrow, "Aé").err().unwrap();

    assert_eq!(wide.chars(), 2);
    assert_eq!(
        error,
        "a single-byte RFC library accepts ASCII only; the text contains other characters; use the \
         NW RFC SDK library (ABAPREACH_RFC_LIBRARY=...\\sapnwrfc.dll) instead"
    );
}

#[test]
fn trace_directory_reaches_a_single_byte_library_as_bytes() {
    let _guard = TRACE_LOCK.lock().unwrap();
    *TRACE_CALLS.lock().unwrap() = None;

    assert_eq!(
        apply_trace_directory_to(
            Some(record_narrow_trace_dir),
            UcWidth::Narrow,
            r"C:\Users\user\AppData\Local\Temp"
        ),
        TraceTarget::Symbol
    );
    // Read as `char*`, the string is whole; read as UTF-16 it would have stopped after `C`.
    assert_eq!(
        TRACE_CALLS.lock().unwrap().clone(),
        Some(r"C:\Users\user\AppData\Local\Temp".to_owned())
    );
}

#[test]
fn trace_directory_is_not_passed_when_a_single_byte_library_cannot_read_it() {
    let _guard = TRACE_LOCK.lock().unwrap();
    *TRACE_CALLS.lock().unwrap() = None;

    assert_eq!(
        apply_trace_directory_to(
            Some(record_narrow_trace_dir),
            UcWidth::Narrow,
            r"C:\Users\José\AppData\Local\Temp"
        ),
        TraceTarget::NotEncodable
    );
    assert_eq!(*TRACE_CALLS.lock().unwrap(), None);
    assert_eq!(
        TraceTarget::NotEncodable.label(),
        "RFC trace directory: RfcSetTraceDir exported, not called (the temp directory is not ASCII \
         and the library uses single-byte characters), environment only"
    );
}

#[test]
fn trace_directory_with_other_characters_still_reaches_a_utf16_library() {
    let _guard = TRACE_LOCK.lock().unwrap();
    *TRACE_CALLS.lock().unwrap() = None;

    assert_eq!(
        apply_trace_directory_to(
            Some(record_trace_dir),
            UcWidth::Utf16,
            r"C:\Users\José\AppData\Local\Temp"
        ),
        TraceTarget::Symbol
    );
    assert_eq!(
        TRACE_CALLS.lock().unwrap().clone(),
        Some(r"C:\Users\José\AppData\Local\Temp".to_owned())
    );
}

const WINDOWS_ICU_SET: [&str; 3] = ["icudt57.dll", "icuuc57.dll", "icuin57.dll"];

#[test]
fn windows_preloads_the_icu_set_beside_the_sdk_library_in_dependency_order() {
    let directory = Path::new("/users/user/sap/nwrfcsdk/lib");
    let library = directory.join("sapnwrfc.dll");
    let files = icu_files(
        directory,
        &[
            "libsapucum.dll",
            "icuin57.dll",
            "sapnwrfc.dll",
            "icuuc57.dll",
            "icudt57.dll",
        ],
    );
    let plan = windows_dependency_preload(&library, &files);

    // Data, common, internationalization, whatever order the directory lists them in.
    assert_eq!(plan.paths, icu_files(directory, &WINDOWS_ICU_SET));
    assert_eq!(plan.note, None);
}

#[test]
fn windows_prefers_the_highest_complete_icu_version_and_ignores_other_files() {
    let directory = Path::new("/sdk/lib");
    let library = directory.join("sapnwrfc.dll");
    let mut files = icu_files(
        directory,
        &[
            "icudt50.dll",
            "icuuc50.dll",
            "icuin50.dll",
            "icudt57.dll",
            "icuuc57.dll",
            "icuin57.dll",
            // Version 60 lacks `icuin`, so it never forms a set.
            "icudt60.dll",
            "icuuc60.dll",
            // Not ICU libraries: a debug build, a different prefix, no version.
            "icuuc57d.dll",
            "icuio57.dll",
            "icuuc.dll",
            "icuuc57.lib",
        ],
    );

    files.extend(icu_files(
        Path::new("/elsewhere"),
        &["icudt70.dll", "icuuc70.dll", "icuin70.dll"],
    ));

    assert_eq!(
        windows_dependency_preload(&library, &files).paths,
        icu_files(directory, &WINDOWS_ICU_SET)
    );
}

#[test]
fn windows_matches_icu_names_the_way_the_file_system_does() {
    let directory = Path::new("/sdk/lib");
    let library = directory.join("SAPNWRFC.DLL");
    let files = icu_files(directory, &["ICUDT57.DLL", "ICUUC57.dll", "icuin57.DLL"]);

    assert_eq!(windows_dependency_preload(&library, &files).paths, files);
}

const MISSING_WINDOWS_ICU_WARNING: &str = "WARNING: RFC ICU libraries were not found beside the RFC library; the NW RFC SDK may still find them through PATH.";

#[test]
fn windows_warns_when_the_sdk_library_has_no_complete_icu_set() {
    let directory = Path::new("/sdk/lib");
    let library = directory.join("sapnwrfc.dll");

    for names in [
        &[][..],
        &["icudt57.dll", "icuuc57.dll"][..],
        &["icudt57.dll", "icuuc57.dll", "icuin60.dll"][..],
    ] {
        let plan = windows_dependency_preload(&library, &icu_files(directory, names));

        assert!(plan.paths.is_empty(), "{names:?}");
        assert_eq!(
            plan.note.as_deref(),
            Some(MISSING_WINDOWS_ICU_WARNING),
            "{names:?}"
        );
    }
}

#[test]
fn windows_gives_the_sap_gui_library_no_icu_warning() {
    // The SAP GUI library ships without ICU and never asks for it.
    let directory = Path::new("/Program Files/SAP/FrontEnd/SAPGUI");
    let library = directory.join("sapnwrfccm.dll");
    let plan = windows_dependency_preload(&library, &icu_files(directory, &["guirfc.dll"]));

    assert!(plan.paths.is_empty());
    assert_eq!(plan.note, None);
}

#[test]
fn windows_preloads_icu_only_for_the_sdk_library_name() {
    let directory = Path::new("/sdk/lib");
    let files = icu_files(directory, &WINDOWS_ICU_SET);

    // A stray ICU set beside the SAP GUI library (or any other name) is not mapped for it.
    for name in ["sapnwrfccm.dll", "other.dll"] {
        let plan = windows_dependency_preload(&directory.join(name), &files);

        assert!(plan.paths.is_empty(), "{name}");
        assert_eq!(plan.note, None, "{name}");
    }
}

/// Windows only: the real loader, with DLL copies of the empty fixture standing in for ICU.
#[cfg(windows)]
mod windows_loader {
    use super::*;

    /// `GetModuleHandle`: whether a bare module name is already mapped, which is what makes a
    /// later `LoadLibrary("icuuc57.dll")` by SAP's library succeed without a search path.
    fn is_mapped(name: &str) -> bool {
        libloading::os::windows::Library::open_already_loaded(name).is_ok()
    }

    fn sdk_directory(icu: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let fixture = built_shared_object(directory.path());
        let library = directory.path().join("sapnwrfc.dll");

        std::fs::copy(&fixture, &library).unwrap();

        for name in icu {
            std::fs::copy(&fixture, directory.path().join(name)).unwrap();
        }

        (directory, library)
    }

    #[test]
    fn the_icu_set_is_mapped_by_name_before_the_library_is() {
        // Unique names: the loader never unloads, so another test must not already hold them.
        let names = ["icudt901.dll", "icuuc901.dll", "icuin901.dll"];
        let (_directory, library) = sdk_directory(&names);

        assert!(names.iter().all(|name| !is_mapped(name)));

        let loaded = RfcLibrary::load(&Env::test_paths(library.to_str().unwrap(), "", ""))
            .unwrap_or_else(|error| panic!("the fixture must load: {error}"));

        assert!(loaded.notes().is_empty(), "{:?}", loaded.notes());
        assert!(names.iter().all(|name| is_mapped(name)));
    }

    #[test]
    fn a_missing_icu_set_loads_the_library_with_the_path_warning() {
        let (_directory, library) = sdk_directory(&[]);
        let loaded = RfcLibrary::load(&Env::test_paths(library.to_str().unwrap(), "", ""))
            .unwrap_or_else(|error| panic!("the fixture must load: {error}"));

        assert_eq!(loaded.notes(), [MISSING_WINDOWS_ICU_WARNING]);
    }
}

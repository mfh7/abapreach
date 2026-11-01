use abapreach::transport::rfc::{
    Arch, Candidate, CandidateFs, ELF_MAGIC, ElfDynamic, Env, Family,
    LOAD_WITH_ALTERED_SEARCH_PATH, LibraryIdentity, LoadError, LoadSegment, MAX_GUI_SEARCH_DEPTH,
    MAX_READ_BYTES, Origin, PE_OFFSET_END, Platform, RFC_TRACE_DIR_ENV, RfcLibrary, SystemFs,
    TraceTarget, apply_trace_directory, bundle_version, compatible_arch, decide, declared_archs,
    dependency_preload, elf_dynamic, family_of, from_sap_uc, gui_library_filename,
    load_address_offset, load_compatible, needs_dependency_preload, preload_dependencies,
    rfc_library_filename, sap_uc, sdk_candidates, sibling_dependencies, trace_directory,
    trace_environment, windows_load_flags,
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

/// An ELF64 image whose dynamic section holds `DT_NEEDED`, `DT_STRTAB`, `DT_STRSZ`, then `DT_NULL`,
/// and one more `DT_NEEDED` past the terminator, so a reader that ignores the terminator sees a
/// dependency the image does not have.
fn elf_with_a_dependency_past_the_terminator(first: &str, past: &str) -> Vec<u8> {
    let mut strings = vec![0u8];
    let mut add = |name: &str| {
        let offset = strings.len() as u64;

        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        offset
    };
    let first_offset = add(first);
    let past_offset = add(past);
    let entries = [
        (1i64, first_offset),
        (5, ELF_STRINGS as u64),
        (10, strings.len() as u64),
        (0, 0),
        (1, past_offset),
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
            "family NW RFC SDK source discovered"
        )
    );

    let gui = LibraryIdentity::new(
        PathBuf::from("/opt/sapgui/bin/libsapnwrfccm.so"),
        "7.50.17".to_owned(),
        Arch::X64,
        Family::Gui,
        Origin::Explicit,
    );

    assert_eq!(
        gui.report(),
        concat!(
            "/opt/sapgui/bin/libsapnwrfccm.so version 7.50.17 architecture x86-64 ",
            "family SAP GUI source explicit ABAPREACH_RFC_LIBRARY"
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

    assert_eq!(found[0].path, cm);
    assert_eq!(found[0].family, Family::Gui);
    assert_eq!(found[0].origin, Origin::Discovered);
    assert_eq!(found[1].path, rfc);
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
fn elf_dynamic_entries_name_dependencies_and_the_run_path() {
    let sdk = dynamic(&elf_with_dynamic(
        &["libsapucum.so", "libicuuc.so.57"],
        Some("/opt/rh/SAP/lib64:/usr/sap/lib"),
        None,
    ))
    .unwrap();

    assert_eq!(sdk.needed(), ["libsapucum.so", "libicuuc.so.57"]);
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
    // An image with neither dependencies nor a search path declares neither.
    let empty = dynamic(&elf_with_dynamic(&[], None, None)).unwrap();

    assert!(empty.needed().is_empty());
    assert!(empty.run_path().is_empty());
    assert!(!empty.declares_origin());
}

#[test]
fn an_unusable_elf_dynamic_section_is_reported() {
    let mut without_dynamic = elf_with_dynamic(&["libsapucum.so"], None, None);

    // No program header means no dynamic section, so nothing names a dependency.
    without_dynamic[0x38..0x3a].copy_from_slice(&0u16.to_le_bytes());

    let mut without_string_table = elf_with_dynamic(&["libsapucum.so"], None, None);
    // One `DT_NEEDED` entry comes first, so `DT_STRTAB` is the second one; clearing its tag leaves
    // every name unresolved.
    let string_table_entry = ELF_DYNAMIC + 16;

    without_string_table[string_table_entry..string_table_entry + 8]
        .copy_from_slice(&0i64.to_le_bytes());

    let mut string_table_outside = elf_with_dynamic(&["libsapucum.so"], None, None);

    string_table_outside[string_table_entry + 8..string_table_entry + 16]
        .copy_from_slice(&0x00ff_ffffu64.to_le_bytes());

    let cases = [
        b"not an ELF image at all".to_vec(),
        elf_with_dynamic(&["libsapucum.so"], None, None)[..0x40].to_vec(),
        without_dynamic,
        without_string_table,
        string_table_outside,
    ];

    for image in cases {
        let error = dynamic(&image).expect_err("an unusable dynamic section is never a decision");

        assert!(!error.is_empty(), "the rejection must carry a reason");
    }
}

#[test]
fn the_sdk_family_or_a_library_without_an_origin_is_preloaded() {
    // Mandatory for the SDK family, whose image has no `$ORIGIN` run path.
    assert!(needs_dependency_preload(Family::Sdk, false));
    assert!(needs_dependency_preload(Family::Sdk, true));
    // The same rule for any other library that lacks `$ORIGIN`.
    assert!(needs_dependency_preload(Family::Gui, false));
    assert!(!needs_dependency_preload(Family::Gui, true));
}

#[test]
fn same_directory_preload_only_includes_dependencies_beside_the_library() {
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
    let library = directory.join("libsapnwrfc.so");
    let entries = dynamic(&elf_with_dynamic(
        &["libsapucum.so", "libicuuc.so.57", "libabsent.so"],
        None,
        None,
    ))
    .unwrap();
    let fs = FakeFs::new(
        &[
            &library,
            &directory.join("libsapucum.so"),
            &directory.join("libicuuc.so.57"),
            &directory.join("libunrelated.so"),
        ],
        &[],
    );

    // The image's own dependency order, absolute paths, never the library itself, never a sibling
    // nothing depends on.
    assert_eq!(
        sibling_dependencies(&library, entries.needed(), &fs),
        vec![
            directory.join("libsapucum.so"),
            directory.join("libicuuc.so.57"),
        ]
    );
    assert!(sibling_dependencies(&library, &[], &fs).is_empty());
}

#[test]
fn dependency_preload_applies_the_origin_rule_and_keeps_the_order() {
    let directory = Path::new("/opt/sapgui");
    let library = directory.join("bin").join("libsapnwrfccm.so");
    let sibling = directory.join("bin").join("libsapucum.so");
    let fs = FakeFs::new(&[&library, &sibling], &[]);
    let without_origin = dynamic(&elf_with_dynamic(&["libsapucum.so"], None, None)).unwrap();
    let with_origin =
        dynamic(&elf_with_dynamic(&["libsapucum.so"], Some("$ORIGIN"), None)).unwrap();

    assert_eq!(
        dependency_preload(&library, Family::Sdk, Some(&without_origin), &fs),
        vec![sibling.clone()]
    );
    assert_eq!(
        dependency_preload(&library, Family::Gui, Some(&without_origin), &fs),
        vec![sibling],
        "a GUI library without $ORIGIN needs the same preload"
    );
    assert!(
        dependency_preload(&library, Family::Gui, Some(&with_origin), &fs).is_empty(),
        "a library that resolves its own folder needs no preload"
    );
    assert!(
        dependency_preload(&library, Family::Sdk, None, &fs).is_empty(),
        "an unreadable dynamic section leaves nothing to preload"
    );
}

#[test]
fn preload_dependencies_maps_each_sibling_by_absolute_path_in_order() {
    let search_path = std::env::var_os("LD_LIBRARY_PATH");
    let directory = Path::new("/opt/sap/nwrfcsdk/lib");
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

    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let system_dll = Path::new(&system_root)
        .join("System32")
        .join("kernel32.dll");

    // No SAP library is needed: the adapter maps a real PE through `LoadLibraryExW`.
    load_candidate(&discovered_sdk(&system_dll))
        .map(|_| ())
        .expect("the adapter must map an absolute DLL");
    assert!(
        load_candidate(&discovered_sdk(
            &Path::new(&system_root).join("System32").join("absent.dll")
        ))
        .is_err(),
        "an absent DLL cannot be mapped"
    );
}

/// An SDK library whose image needs a sibling that sits beside it but is not a shared object, so
/// the preload must fail on it, naming the absolute path, before the library itself is mapped.
#[cfg(target_os = "linux")]
fn library_with_an_unloadable_sibling() -> (PathBuf, PathBuf, String) {
    use abapreach::transport::rfc::load_candidate;

    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("libsapnwrfc.so");
    let sibling = directory.path().join("libsapucum.so");

    std::fs::write(&library, elf_with_dynamic(&["libsapucum.so"], None, None)).unwrap();
    std::fs::write(&sibling, b"not a shared object").unwrap();

    let error = load_candidate(&discovered_sdk(&library))
        .map(|_| ())
        .expect_err("the SDK family preloads its dependencies before it maps the library");

    // The temporary directory goes away with the tuple, so the assertion happens in the caller.
    (library, sibling, error)
}

#[cfg(target_os = "linux")]
#[test]
fn linux_adapter_preloads_a_sibling_dependency_by_absolute_path() {
    let (_, sibling, error) = library_with_an_unloadable_sibling();

    assert!(
        error.contains(&sibling.display().to_string()),
        "the preload must name the absolute sibling path: {error}"
    );
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

    let (_, sibling, error) = library_with_an_unloadable_sibling();

    assert!(
        error.contains(&sibling.display().to_string()),
        "the preload must not depend on the search path: {error}"
    );
    // The adapter reads the search path but never rewrites it for this process.
    assert_eq!(std::env::var("LD_LIBRARY_PATH").unwrap_or_default(), "");
}

/// The load-address lookup the ELF string table goes through: the first segment that contains the
/// address maps it, and a segment maps `[address, address + size)`, not one byte more.
#[test]
fn a_load_address_maps_through_the_segment_that_contains_it() {
    let loads = [
        LoadSegment {
            address: 0x1000,
            size: 0x100,
            offset: 0x2000,
        },
        LoadSegment {
            address: 0x4000,
            size: 0x400,
            offset: 0x5000,
        },
    ];

    assert_eq!(load_address_offset(&loads, 0x1000), Some(0x2000));
    assert_eq!(load_address_offset(&loads, 0x1080), Some(0x2080));
    // The last address the segment maps is inside it; the first address past it is not.
    assert_eq!(load_address_offset(&loads, 0x10ff), Some(0x20ff));
    assert_eq!(load_address_offset(&loads, 0x1100), None);
    // A later segment is found only when the earlier one does not contain the address.
    assert_eq!(load_address_offset(&loads, 0x4100), Some(0x5100));
    // An address below every segment is outside the image.
    assert_eq!(load_address_offset(&loads, 0x0fff), None);
    assert_eq!(load_address_offset(&[], 0x1000), None);
}

/// A dynamic section is a sequence of entries, and `DT_NULL` ends it: the entries after the terminator
/// belong to nothing, so a dependency past it is never read.
#[test]
fn a_dynamic_section_stops_at_its_null_entry() {
    let image = elf_with_a_dependency_past_the_terminator("libsapucum.so", "liblater.so");
    let entries = dynamic(&image).unwrap();

    assert_eq!(entries.needed(), ["libsapucum.so"]);
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

/// A section longer than the reader accepts is refused before any buffer is allocated.
#[test]
fn a_section_longer_than_the_read_limit_is_refused() {
    let mut image = elf_with_dynamic(&["libsapucum.so"], None, None);

    // `DT_STRSZ` is the last entry before the terminator.
    let strings_size = ELF_DYNAMIC + 2 * 16 + 8;

    image[strings_size..strings_size + 8].copy_from_slice(&(MAX_READ_BYTES + 1).to_le_bytes());

    let error = dynamic(&image).expect_err("an implausible section length is never read");

    assert!(error.contains("implausible section length"), "{error}");
}

/// A section of exactly the read limit is read; the limit itself is not the rejection. The string
/// table keeps its first name at offset 0, so the padded image still resolves its dependency.
#[test]
fn a_section_of_exactly_the_read_limit_is_read() {
    let mut image = elf_with_dynamic(&["libsapucum.so"], None, None);

    image.resize(ELF_STRINGS + MAX_READ_BYTES as usize, 0);
    // `DT_STRSZ` is the third entry: one dependency, then the string table address and size.
    let strings_size = ELF_DYNAMIC + 2 * 16 + 8;

    image[strings_size..strings_size + 8].copy_from_slice(&MAX_READ_BYTES.to_le_bytes());

    let entries = dynamic(&image).expect("a section of exactly the limit is readable");

    assert_eq!(entries.needed(), ["libsapucum.so"]);
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

use abapreach::transport::rfc::{
    Arch, Candidate, CandidateFs, ELF_MAGIC, Env, LoadError, MAX_GUI_SEARCH_DEPTH, Origin,
    PE_OFFSET_END, Platform, RFC_TRACE_DIR_ENV, RfcLibrary, SystemFs, TraceTarget,
    apply_trace_directory, bundle_version, compatible_arch, decide, declared_archs, from_sap_uc,
    gui_library_filename, load_compatible, rfc_library_filename, sap_uc, sdk_candidates,
    trace_directory, trace_environment,
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
        .map(|(path, origin)| Candidate {
            path: PathBuf::from(path),
            origin: *origin,
        })
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

    assert_eq!(
        candidates(&env),
        oracle(&[
            ("/override", Origin::Explicit),
            ("/gui", Origin::Discovered),
            ("/sdk", Origin::Discovered),
        ])
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

    let error = load_compatible(&foreign).expect_err("an unreadable header is never mapped");

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

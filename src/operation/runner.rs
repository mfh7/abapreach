use crate::transport::TransportError;

/// The product default; the live test harness selects its own prefix through `runner_prefix`.
pub const DEFAULT_RUNNER_PREFIX: &str = "YCL_ABAPREACH_RUN_";

const SUFFIX_LENGTH: usize = 8;
const MAX_PREFIX_LENGTH: usize = 22;
const SUFFIX_ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// The largest multiple of 36 that fits a byte: a byte at or above it is redrawn, so every
/// alphabet character is equally likely.
const BIAS_LIMIT: u8 = 252;

/// The `runner_prefix` rule: upper-case letters, digits, and underscore, starting with `Y` or
/// `Z`, at most 22 characters, so the name stays within the 30 characters of a class name.
pub fn validate_runner_prefix(prefix: &str) -> Result<(), String> {
    if !prefix.starts_with(['Y', 'Z']) {
        return Err("must start with Y or Z".to_owned());
    }
    if prefix.len() > MAX_PREFIX_LENGTH {
        return Err(format!("must be at most {MAX_PREFIX_LENGTH} characters"));
    }
    if !prefix
        .bytes()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err("may contain only upper-case letters, digits, and underscore".to_owned());
    }

    Ok(())
}

/// A fresh runner class name: `prefix` plus eight `[0-9A-Z]` characters from the OS random source.
pub fn runner_name(prefix: &str) -> String {
    runner_name_with(prefix, &mut |buffer| {
        getrandom::fill(buffer).expect("the OS random source is available")
    })
}

/// [`runner_name`] over any byte source, so the mapping is tested on every platform.
pub fn runner_name_with(prefix: &str, fill: &mut dyn FnMut(&mut [u8])) -> String {
    let mut name = String::from(prefix);
    let mut drawn = 0;

    while drawn < SUFFIX_LENGTH {
        let mut bytes = [0u8; SUFFIX_LENGTH];

        fill(&mut bytes);
        for byte in bytes.into_iter().filter(|byte| *byte < BIAS_LIMIT) {
            if drawn < SUFFIX_LENGTH {
                name.push(char::from(SUFFIX_ALPHABET[usize::from(byte) % 36]));
                drawn += 1;
            }
        }
    }

    name
}

/// Why a runner class could not be created.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    /// Both names were taken (HTTP 409); nothing was written.
    #[error("runner class name collided twice; retry later")]
    TwoCollisions,
    #[error("{0}")]
    Failed(TransportError),
}

/// Creates the runner class under a fresh name. A 409 means the name exists and nothing was
/// written, so one new name is tried; a second 409 or any other failure ends it.
pub fn create_with_fresh_name(
    mut new_name: impl FnMut() -> String,
    mut create: impl FnMut(&str) -> Result<(), TransportError>,
) -> Result<String, CreateError> {
    for attempt in 0..2 {
        let name = new_name();

        match create(&name) {
            Ok(()) => return Ok(name),
            Err(TransportError::Http { status: 409, .. }) if attempt == 0 => {}
            Err(TransportError::Http { status: 409, .. }) => {
                return Err(CreateError::TwoCollisions);
            }
            Err(error) => return Err(CreateError::Failed(error)),
        }
    }

    unreachable!("the second attempt always returns")
}

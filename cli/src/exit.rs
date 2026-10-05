//! Exit codes, `CLI_SPEC.md` §1.
//!
//! Distinct codes exist so a CI script can tell "the download broke" from "the
//! package is not trustworthy". A script that cannot separate those cannot
//! enforce a security policy, and merging them into a single `1` is the usual
//! reason verification failures get treated as flakes and retried.

use std::process::ExitCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Success.
    Ok,
    /// Runtime failure: network, filesystem, corrupt data.
    Failure,
    /// Usage error: unknown flag, missing argument, bad ISO code.
    Usage,
    /// Verification failure — checksum, signature or schema mismatch.
    ///
    /// Deliberately distinct from `Failure`. This is the code that must fail a
    /// build and must never be retried.
    Verification,
    /// Nothing found. A search with no matches is not an error, but a script
    /// needs to tell "empty result" from "the search broke".
    NotFound,
    /// Insufficient disk space.
    NoSpace,
    /// A required package is not installed.
    NotInstalled,
}

impl Exit {
    pub fn code(self) -> u8 {
        match self {
            Exit::Ok => 0,
            Exit::Failure => 1,
            Exit::Usage => 2,
            Exit::Verification => 3,
            Exit::NotFound => 4,
            Exit::NoSpace => 5,
            Exit::NotInstalled => 6,
        }
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        ExitCode::from(exit.code())
    }
}

/// A command's outcome: an exit code plus a message that belongs on stderr.
pub type Outcome = Result<(), Exit>;

/// Report a usage error and return the code. Kept as a helper because every
/// usage error has the same shape and they all need to land on stderr so stdout
/// stays parseable.
pub fn usage_error(message: impl AsRef<str>) -> Outcome {
    eprintln!("terrakit: {}", message.as_ref());
    eprintln!("Try 'terrakit --help'.");
    Err(Exit::Usage)
}

pub fn failure(message: impl AsRef<str>) -> Outcome {
    eprintln!("terrakit: {}", message.as_ref());
    Err(Exit::Failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_the_specification() {
        assert_eq!(Exit::Ok.code(), 0);
        assert_eq!(Exit::Failure.code(), 1);
        assert_eq!(Exit::Usage.code(), 2);
        assert_eq!(Exit::Verification.code(), 3);
        assert_eq!(Exit::NotFound.code(), 4);
        assert_eq!(Exit::NoSpace.code(), 5);
        assert_eq!(Exit::NotInstalled.code(), 6);
    }

    #[test]
    fn verification_is_distinct_from_failure() {
        // The whole reason these codes are split. A CI policy that keys on
        // "verification failed" cannot be written if both are 1.
        assert_ne!(Exit::Verification.code(), Exit::Failure.code());
    }
}

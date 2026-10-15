//! Process exit codes.

use std::process::ExitCode;

pub const SUCCESS: ExitCode = ExitCode::SUCCESS;

/// Something went wrong while doing the work: the API couldn't be reached or refused the
/// request (a task is queued for retry in that case), or a file couldn't be written.
pub const FAILURE: u8 = 1;

/// The command can't be run as given: a missing token, an invalid task, or a bad query.
/// Retrying without changing something won't help.
pub const USAGE: u8 = 2;

pub fn failure() -> ExitCode {
    ExitCode::from(FAILURE)
}

pub fn usage() -> ExitCode {
    ExitCode::from(USAGE)
}

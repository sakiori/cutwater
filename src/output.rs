//! Standard output. `println!` panics when the reader closed the pipe, as
//! `cutwater status | head -1` does; these writes return the error instead,
//! and the command line ends quietly on it.

use std::fmt;
use std::io::{self, Write};

/// Writes a line to standard output.
pub fn line(args: fmt::Arguments) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_fmt(args)?;
    out.write_all(b"\n")
}

/// Whether an error is a write to a closed pipe.
pub fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|err| err.kind() == io::ErrorKind::BrokenPipe)
    })
}

/// Writes a line to standard output, like `println!`, and evaluates to its
/// `io::Result`.
#[macro_export]
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::output::line(format_args!($($arg)*))
    };
}

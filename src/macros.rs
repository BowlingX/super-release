//! Small stdout helpers pulled in with `#[macro_use]` from the crate root so they are available crate-wide.

/// Print and immediately flush to ensure output is visible.
macro_rules! printfl {
    ($($arg:tt)*) => {{
        println!($($arg)*);
        use ::std::io::Write as _;
        let _ = ::std::io::stdout().flush();
    }};
}

/// Execute a block or print a line only in verbose mode.
macro_rules! verbosefl {
    ($verbose:expr) => {{ if $verbose { printfl!(); } }};
    ($verbose:expr, { $($body:tt)* }) => {{
        if $verbose { $($body)* }
    }};
    ($verbose:expr, $($arg:tt)*) => {{
        if $verbose { printfl!($($arg)*); }
    }};
}

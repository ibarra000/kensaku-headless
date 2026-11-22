//! Print macros that ignore write failures (closed pipe/console), since std
//! `println!`/`eprintln!` panic and would kill background threads.

#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stdout(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! say_err {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr(), $($arg)*);
    }};
}

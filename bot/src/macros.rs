/// Helper macro to log an error and all it's causes.
#[macro_export]
macro_rules! log_err {
    ($e:expr, $fmt:expr $(, $($arg:tt)*)?) => {{
        let e = failure::Error::from($e);

        log::error!("{what}: {error}", what = format!($fmt $(, $($arg)*)*), error = e);
        log::error!("backtrace: {}", e.backtrace());

        for cause in e.iter_causes() {
            log::error!("caused by: {}", cause);
            log::error!("backtrace: {}", e.backtrace());
        }
    }};
}

/// Helper macro to handle the result of polling an infinite stream.
#[macro_export]
macro_rules! try_infinite {
    ($expr:expr) => {
        match $expr {
            Err(e) => return Err(e.into()),
            Ok(a) => match a {
                futures::Async::NotReady => None,
                futures::Async::Ready(None) => failure::bail!("stream ended unexpectedly"),
                futures::Async::Ready(Some(v)) => Some(v),
            },
        }
    };
}

/// Helper macro to handle the result of polling an infinite stream that can error with a unit.
#[macro_export]
macro_rules! try_infinite_empty {
    ($expr:expr) => {
        try_infinite!($expr.map_err(|()| failure::format_err!("stream unexpectedly errored")))
    };
}
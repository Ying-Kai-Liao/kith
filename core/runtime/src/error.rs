use std::fmt;

/// Why the core would not open, or refused a write. The text says what to do about it.
#[derive(Debug)]
pub enum Error {
    /// The core folder or one of its files is open to other users (or, on Windows, to other accounts).
    Permissions(String),
    /// The marker says a core.db existed here, and there is none.
    DatabaseMissing(String),
    /// core.db is not the database the marker recorded.
    DatabaseSwapped(String),
    /// `quick_check` or `integrity_check` failed.
    Corrupt(String),
    /// core.db's schema is older than this binary's, and this entry point does not migrate.
    NeedsMigration(String),
    /// core.db's schema is newer than this binary knows.
    BinaryOlder(String),
    /// The lock is held by a running core (`start` and `migrate` need it for themselves).
    AlreadyRunning(String),
    /// A claim transaction was opened by a core whose generation is no longer the newest.
    StaleGeneration(String),
    /// A file or database call failed.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::Permissions(m)
        | Self::DatabaseMissing(m)
        | Self::DatabaseSwapped(m)
        | Self::Corrupt(m)
        | Self::NeedsMigration(m)
        | Self::BinaryOlder(m)
        | Self::AlreadyRunning(m)
        | Self::StaleGeneration(m)
        | Self::Io(m)) = self;
        f.write_str(m)
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Io(format!("core.db: {e}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

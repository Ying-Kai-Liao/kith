use std::fmt;

/// Why the core would not open, or refused a write. The text says what to do about it.
#[derive(Clone, Debug)]
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
    /// The authority rules could not answer from the tables as they are (a membership names a preset nobody defined, a
    /// column holds text that is not JSON). The file is not damaged, so nothing here says to restore a backup (which
    /// could undo a later revoke); the caller reads it as "no".
    Refused(String),
    /// [`crate::Core::call`]: the table has no such method.
    NoSuchMethod(String),
    /// [`crate::Core::call`]: the method is not open to this principal.
    NotAllowed(String),
    /// [`crate::Core::call`]: the params do not fit the method's row.
    BadParams(String),
    /// [`crate::Core::call`]: the method is in the table but nothing in the core answers it yet (a Python verb, or one the
    /// daemon's wiring registers at M0c). The caller does what it did before the table.
    NotServed(String),
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
        | Self::Io(m)
        | Self::Refused(m)
        | Self::NoSuchMethod(m)
        | Self::NotAllowed(m)
        | Self::BadParams(m)
        | Self::NotServed(m)) = self;
        f.write_str(m)
    }
}

impl Error {
    /// The recovery verb for a core that will not run (runtime-design, "The core-down list", item 5): the verbs that
    /// keep working when no core can. Every other verb refuses with a status that names one of these.
    pub fn recovery(&self) -> &'static str {
        match self {
            Self::Corrupt(_) | Self::DatabaseMissing(_) | Self::DatabaseSwapped(_) => {
                "kith restore"
            }
            Self::NeedsMigration(_) => "kith migrate",
            Self::BinaryOlder(_) => "kith update",
            // A silent lock holder, a mode that is too open, a file that will not open: the doctor says which.
            _ => "kith doctor",
        }
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

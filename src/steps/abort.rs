//! What a start undoes when it fails (R164).

use super::{Ledger, OpenedFiles, Rollback};

/// What a failed sequence undoes (R164): each change in the [`Ledger`], in
/// the reverse of the order the steps made them, then the owners step 16
/// replaced, and last the lock (step 7). A half-done identity switch (step 17)
/// undoes itself. A panic in foreground mode, unwinding past the sequence,
/// rolls back as the guard drops, with nowhere to report; a daemon catches
/// its panic and rolls back as for an error (R166).
///
/// In foreground mode the process is the caller's, and it gets back the parts
/// that can be restored. In daemon mode the process exits on failure, so only
/// the tokens for files on disk hold anything. What cannot be restored is in
/// R164. Step 18 hands over the lock. In foreground mode it keeps every
/// change; in daemon mode the `DaemonContext` takes the guard, to roll back a
/// failure before readiness (R170).
///
/// The pidfile goes while the lock is still held: released first, the lock
/// would let a second instance in to write its pid into a file this one then
/// removes. [`Drop`] rolls the ledger back in its body, and `files`, the
/// lock among them, drops after the body.
pub(crate) struct AbortGuard {
    /// What the steps changed.
    pub(crate) ledger: Ledger,
    /// The files steps 7, 8 and 12 opened, the lock among them.
    pub(crate) files: OpenedFiles,
}

impl AbortGuard {
    /// Puts back what the steps changed and returns what it could not put
    /// back. The owners step 16 replaced go after the ledger: a file the
    /// ledger removed needs no owner, and a daemon switched to a user could
    /// not give it one.
    ///
    /// Flushes stdout first while the ledger holds a change: what init left
    /// in its buffer belongs in the log, and decides whether the rollback
    /// keeps it. A start kept whole leaves the caller's buffer alone.
    pub(crate) fn roll_back(&mut self) -> Vec<crate::RestoreFailure> {
        if !self.ledger.is_empty() {
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        let mut failures = self.ledger.roll_back();
        failures.extend(self.files.give_owners_back());
        failures
    }

    /// Keeps every change: the ledger's, and the owners step 16 gave.
    pub(crate) fn keep(&mut self) {
        self.ledger.keep_all();
        self.files.keep_owners();
    }

    pub(crate) fn new(foreground: bool) -> Self {
        let rollback = if foreground {
            Rollback::PutBack
        } else {
            Rollback::Skip
        };
        Self {
            ledger: Ledger::new(rollback),
            files: OpenedFiles::default(),
        }
    }
}

impl Drop for AbortGuard {
    fn drop(&mut self) {
        // Empty unless a foreground panic unwound past `run_post_fork`, or a
        // `DaemonContext` let go of a start it neither kept nor rolled back:
        // neither leaves an error to report the failures in. Here, not by
        // field order, so the pidfile goes before the lock whatever order
        // the fields are in.
        let _ = self.roll_back();
    }
}

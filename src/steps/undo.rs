//! Undo tokens: what a step changed, put back when the token drops (R164).
//!
//! A step that changes state a caller can see returns an [`Undo`], so the
//! change and its undo are one value: a step cannot make one without the
//! other. The sequence holds the tokens in the order the steps ran and drops
//! them in reverse on failure; at step 18 it keeps them all.

use std::os::fd::RawFd;

/// Whether a step saves what it changes, so a failure can put it back
/// (R147, R164).
#[derive(Clone, Copy)]
pub(crate) enum Rollback {
    /// Save the state before changing it, and restore it on failure.
    PutBack,
    /// Change the state without saving it. For a daemon child, which exits on
    /// any failure, so no one sees the state afterward.
    Skip,
}

/// How to put back one change.
pub(crate) trait Restore {
    /// Puts the state back.
    fn restore(self: Box<Self>);

    /// A descriptor this holds, which step 13's sweep must spare.
    fn held_fd(&self) -> Option<RawFd> {
        None
    }
}

/// A change a step made, put back when this drops unless [`keep`](Self::keep)
/// was called.
#[must_use = "a dropped token puts its change back at once"]
pub(crate) struct Undo {
    restore: Option<Box<dyn Restore>>,
    /// Restoring calls `setenv`, which is undefined behaviour once another
    /// thread may run.
    single_threaded: bool,
}

impl Undo {
    /// A token that puts back what `restore` saved.
    pub(crate) fn new(restore: impl Restore + 'static) -> Self {
        Self {
            restore: Some(Box::new(restore)),
            single_threaded: false,
        }
    }

    /// Under [`Rollback::PutBack`], a token from what `save` saves. Under
    /// [`Rollback::Skip`], `save` does not run, and the token restores nothing.
    pub(crate) fn saving<R: Restore + 'static>(
        rollback: Rollback,
        save: impl FnOnce() -> R,
    ) -> Self {
        match rollback {
            Rollback::PutBack => Self::new(save()),
            Rollback::Skip => Self::nothing(),
        }
    }

    /// A token for a step that changed nothing.
    pub(crate) fn nothing() -> Self {
        Self {
            restore: None,
            single_threaded: false,
        }
    }

    /// Marks this token's restore as unsound once other threads may run.
    pub(crate) fn single_threaded(mut self) -> Self {
        self.single_threaded = true;
        self
    }

    /// Other threads may run from here, so a single-threaded token restores
    /// nothing.
    pub(crate) fn threads_may_start(&mut self) {
        if self.single_threaded {
            self.restore = None;
        }
    }

    /// Leaves the change in place.
    pub(crate) fn keep(mut self) {
        self.restore = None;
    }

    /// The descriptor this holds, which step 13's sweep must spare.
    pub(crate) fn held_fd(&self) -> Option<RawFd> {
        self.restore.as_ref().and_then(|r| r.held_fd())
    }
}

impl Drop for Undo {
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            restore.restore();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steps::{change_dir, set_env_vars, set_umask};
    use crate::test_support::isolated;
    use nix::sys::stat::{Mode, umask};

    /// The umask now; reading it means setting it, so this puts it back.
    fn current_umask() -> Mode {
        let now = umask(Mode::empty());
        umask(now);
        now
    }

    #[test]
    fn a_token_holding_no_descriptor_spares_none() {
        if !isolated() {
            return;
        }
        let undo = set_umask(0o077, Rollback::PutBack);
        assert_eq!(undo.held_fd(), None);
    }

    #[test]
    fn a_token_holding_a_descriptor_spares_it() {
        if !isolated() {
            return;
        }
        let undo = change_dir(std::path::Path::new("/"), Rollback::PutBack).unwrap();
        assert!(undo.held_fd().is_some());
    }

    #[test]
    fn a_dropped_token_restores() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(0o022));
        drop(set_umask(0o077, Rollback::PutBack));
        assert_eq!(current_umask().bits(), 0o022);
    }

    #[test]
    fn a_kept_token_restores_nothing() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(0o022));
        set_umask(0o077, Rollback::PutBack).keep();
        assert_eq!(current_umask().bits(), 0o077);
    }

    #[test]
    fn under_skip_nothing_is_restored() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(0o022));
        drop(set_umask(0o077, Rollback::Skip));
        assert_eq!(current_umask().bits(), 0o077);
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // setenv: the isolated child runs this test alone
    fn a_single_threaded_token_restores_nothing_once_threads_may_start() {
        if !isolated() {
            return;
        }
        let key = "BLIVET_UNDO_TEST";
        let mut undo = set_env_vars(&[(key.into(), "set".into())], Rollback::PutBack);
        undo.threads_may_start();
        drop(undo);
        assert_eq!(std::env::var(key).as_deref(), Ok("set"));
    }

    #[test]
    fn other_tokens_still_restore_once_threads_may_start() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(0o022));
        let mut undo = set_umask(0o077, Rollback::PutBack);
        undo.threads_may_start();
        drop(undo);
        assert_eq!(current_umask().bits(), 0o022);
    }
}

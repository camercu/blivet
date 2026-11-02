//! Undo tokens: what a step changed, put back when the token drops (R164).
//!
//! A step that changes state a caller can see returns an [`Undo`], so the
//! change and its undo are one value: a step cannot make one without the
//! other. The sequence holds the tokens in the order the steps ran. On
//! failure it restores them in reverse and reports each that could not be put
//! back; at step 18 it keeps them all. A token that drops unrestored puts its
//! change back and reports nothing.
//!
//! Most tokens hold process state, which a daemon's exit discards, so a
//! daemon saves none ([`Rollback::Skip`]). A file a step left on disk outlives
//! the process: its token is made with [`Undo::new`] in both modes.

use std::os::fd::RawFd;

use crate::error::RestoreFailure;

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

/// What one step saved before its change, to put back.
///
/// Every match on this is exhaustive, so a new kind of saved state must say
/// how it restores, which descriptor it holds, and whether its restore is
/// sound once other threads may run.
pub(crate) enum Saved {
    Umask(super::Umask),
    Cwd(super::Cwd),
    SignalMask(super::SignalMask),
    EnvVars(super::EnvVars),
    Dispositions(crate::signals::Dispositions),
    Slot(super::SavedSlot),
    Groups(crate::identity::Groups),
    Removal(super::Removal),
    Owner(crate::identity::Owner),
    Emptied(super::Emptied),
}

impl Saved {
    /// Puts the state back.
    fn restore(self) -> Result<(), RestoreFailure> {
        match self {
            Self::Umask(s) => {
                s.restore();
                Ok(())
            }
            Self::Cwd(s) => s.restore(),
            Self::SignalMask(s) => s.restore(),
            Self::EnvVars(s) => {
                s.restore();
                Ok(())
            }
            Self::Dispositions(s) => s.restore(),
            Self::Slot(s) => s.restore(),
            Self::Groups(s) => s.restore(),
            Self::Removal(s) => s.restore(),
            Self::Owner(s) => s.restore(),
            Self::Emptied(s) => s.restore(),
        }
    }

    /// A descriptor this holds, which step 13's sweep must spare.
    fn held_fd(&self) -> Option<RawFd> {
        match self {
            Self::Cwd(s) => s.held_fd(),
            Self::Slot(s) => s.held_fd(),
            Self::Umask(_)
            | Self::SignalMask(_)
            | Self::EnvVars(_)
            | Self::Dispositions(_)
            | Self::Groups(_)
            | Self::Removal(_) => None,
            // Made at steps 15 and 16, after the sweep.
            Self::Owner(_) | Self::Emptied(_) => None,
        }
    }

    /// Whether restoring is unsound once other threads may run. Restoring
    /// the env calls `setenv`, which is undefined behaviour then.
    fn single_threaded(&self) -> bool {
        match self {
            Self::EnvVars(_) => true,
            Self::Umask(_)
            | Self::Cwd(_)
            | Self::SignalMask(_)
            | Self::Dispositions(_)
            | Self::Slot(_)
            | Self::Groups(_)
            | Self::Removal(_)
            | Self::Owner(_)
            | Self::Emptied(_) => false,
        }
    }
}

/// A change a step made, put back when this drops unless [`keep`](Self::keep)
/// was called.
#[must_use = "a dropped token puts its change back at once"]
pub(crate) struct Undo(Option<Saved>);

impl Undo {
    /// A token that puts back what `saved` holds.
    pub(crate) fn new(saved: Saved) -> Self {
        Self(Some(saved))
    }

    /// Under [`Rollback::PutBack`], a token from what `save` saves. Under
    /// [`Rollback::Skip`], `save` does not run, and the token restores nothing.
    pub(crate) fn saving(rollback: Rollback, save: impl FnOnce() -> Saved) -> Self {
        match rollback {
            Rollback::PutBack => Self::new(save()),
            Rollback::Skip => Self::nothing(),
        }
    }

    /// A token for a step that changed nothing.
    pub(crate) fn nothing() -> Self {
        Self(None)
    }

    /// Other threads may run from here, so a single-threaded token restores
    /// nothing.
    pub(crate) fn threads_may_start(&mut self) {
        if self.0.as_ref().is_some_and(Saved::single_threaded) {
            self.0 = None;
        }
    }

    /// Puts the change back now, unless it was kept.
    pub(crate) fn restore(mut self) -> Result<(), RestoreFailure> {
        self.0.take().map_or(Ok(()), Saved::restore)
    }

    /// Leaves the change in place.
    pub(crate) fn keep(mut self) {
        self.0 = None;
    }

    /// The descriptor this holds, which step 13's sweep must spare.
    pub(crate) fn held_fd(&self) -> Option<RawFd> {
        self.0.as_ref().and_then(Saved::held_fd)
    }
}

impl Drop for Undo {
    fn drop(&mut self) {
        if let Some(saved) = self.0.take() {
            // A token restored by dropping has no one to report to.
            let _ = saved.restore();
        }
    }
}

/// Puts back what `tokens` hold, latest first, and returns what could not
/// be put back. For a step that fails part way, and for the sequence.
pub(crate) fn restore_all(tokens: Vec<Undo>) -> Vec<RestoreFailure> {
    tokens
        .into_iter()
        .rev()
        .filter_map(|undo| undo.restore().err())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steps::{change_dir, set_env_vars, set_umask};
    use crate::test_support::{CALLER_UMASK, current_umask, isolated};
    use nix::sys::stat::{Mode, umask};

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
        umask(Mode::from_bits_truncate(CALLER_UMASK));
        drop(set_umask(0o077, Rollback::PutBack));
        assert_eq!(current_umask().bits(), CALLER_UMASK);
    }

    #[test]
    fn a_kept_token_restores_nothing() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(CALLER_UMASK));
        set_umask(0o077, Rollback::PutBack).keep();
        assert_eq!(current_umask().bits(), 0o077);
    }

    #[test]
    fn under_skip_nothing_is_saved() {
        Undo::saving(Rollback::Skip, || unreachable!("Skip must not save")).keep();
    }

    #[test]
    fn under_skip_nothing_is_restored() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(CALLER_UMASK));
        drop(set_umask(0o077, Rollback::Skip));
        assert_eq!(current_umask().bits(), 0o077);
    }

    #[test]
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
        umask(Mode::from_bits_truncate(CALLER_UMASK));
        let mut undo = set_umask(0o077, Rollback::PutBack);
        undo.threads_may_start();
        drop(undo);
        assert_eq!(current_umask().bits(), CALLER_UMASK);
    }
}

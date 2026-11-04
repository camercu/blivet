//! Undo tokens: what a step changed, and how to put it back (R164).
//!
//! A step that changes state a caller can see makes an [`Undo`], so the
//! change and its undo are one value: a step cannot make one without the
//! other. Every step records each token in the [`Ledger`] as it makes the
//! change, so a failure part way leaves nothing unrecorded. The ledger holds
//! the tokens in the order the changes were made. On failure it restores
//! them in reverse and reports each that could not be put back; at step 18
//! it keeps them all. Dropped unkept, as by a panic, it restores them and
//! reports nothing. A token holds no rollback of its own: dropped outside
//! the ledger, it puts nothing back.
//!
//! Most tokens hold process state, which a daemon's exit discards, so a
//! daemon saves none ([`Ledger::record_saving`]). A file a step left on disk
//! outlives the process: its token goes in with [`Ledger::record_lasting`],
//! in both modes.

use std::os::fd::RawFd;

use crate::error::{ChangedState, ErrorKind, RestoreFailure};

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
    /// A change that can no longer be put back, reported on rollback.
    Left(RestoreFailure),
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
            Self::Left(failure) => Err(failure),
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
            Self::Owner(_) | Self::Emptied(_) | Self::Left(_) => None,
        }
    }

    /// The state whose restore is unsound once other threads may run.
    /// Restoring the env calls `setenv`, which is undefined behaviour then.
    fn single_threaded(&self) -> Option<ChangedState> {
        match self {
            Self::EnvVars(_) => Some(ChangedState::Environment),
            Self::Umask(_)
            | Self::Cwd(_)
            | Self::SignalMask(_)
            | Self::Dispositions(_)
            | Self::Slot(_)
            | Self::Groups(_)
            | Self::Removal(_)
            | Self::Owner(_)
            | Self::Emptied(_)
            | Self::Left(_) => None,
        }
    }
}

/// A change a step made, put back by [`restore`](Self::restore). Dropped, it
/// puts nothing back: a step records each token in the [`Ledger`] as it makes
/// the change, and the ledger is what rolls back.
#[must_use = "a dropped token's change is never put back"]
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
    /// nothing, and reports the change it leaves.
    pub(crate) fn threads_may_start(&mut self) {
        if let Some(state) = self.0.as_ref().and_then(Saved::single_threaded) {
            let why = "init may have started threads, and setenv is not thread-safe";
            self.0 = Some(Saved::Left(RestoreFailure::new(
                state,
                std::io::Error::other(why),
            )));
        }
    }

    /// Puts the change back now.
    pub(crate) fn restore(self) -> Result<(), RestoreFailure> {
        self.0.map_or(Ok(()), Saved::restore)
    }

    /// The descriptor this holds, which step 13's sweep must spare.
    pub(crate) fn held_fd(&self) -> Option<RawFd> {
        self.0.as_ref().and_then(Saved::held_fd)
    }
}

/// The sequence's record of what its steps changed (R164): each step's
/// tokens, in the order the steps ran.
///
/// A step records each change as it makes it, so a step that fails part way
/// leaves the changes it made in here, and [`roll_back`](Self::roll_back)
/// puts them back with the rest. Dropped unkept, it rolls back too, latest
/// first, reporting nothing: for a panic, which leaves no error to report in.
pub(crate) struct Ledger {
    /// `PutBack` in foreground mode, `Skip` in a daemon: the one place the
    /// sequence decides how much a failure undoes.
    rollback: Rollback,
    /// Each change's token, in the order the steps made them.
    undo: Vec<Undo>,
    /// The checkpoints passed, where the undo walk may fail the sequence.
    #[cfg(test)]
    checkpoints: usize,
}

impl Ledger {
    pub(crate) fn new(rollback: Rollback) -> Self {
        Self {
            rollback,
            undo: Vec::new(),
            #[cfg(test)]
            checkpoints: 0,
        }
    }

    /// Whether steps save what they change.
    pub(crate) fn rollback(&self) -> Rollback {
        self.rollback
    }

    /// Holds a change's token, to put back on failure. One that restores
    /// nothing is not kept.
    pub(crate) fn record(&mut self, undo: Undo) -> Result<(), ErrorKind> {
        if undo.0.is_some() {
            self.undo.push(undo);
        }
        self.checkpoint()
    }

    /// Records what `save` saves, under [`Rollback::PutBack`]; in a daemon
    /// `save` does not run. For process state, which a daemon's exit
    /// discards.
    pub(crate) fn record_saving(&mut self, save: impl FnOnce() -> Saved) -> Result<(), ErrorKind> {
        self.record(Undo::saving(self.rollback, save))
    }

    /// Records a change that outlives the process, a file on disk, to put
    /// back in both modes.
    pub(crate) fn record_lasting(&mut self, saved: Saved) -> Result<(), ErrorKind> {
        self.record(Undo::new(saved))
    }

    /// A point the undo walk may fail the sequence at; nothing outside tests.
    pub(crate) fn checkpoint(&mut self) -> Result<(), ErrorKind> {
        #[cfg(test)]
        {
            self.checkpoints += 1;
            if super::failpoints::checkpoint_fails(self.checkpoints) {
                return Err(ErrorKind::SystemError(
                    super::failpoints::CHECKPOINT_FAILURE.into(),
                ));
            }
        }
        Ok(())
    }

    /// From `init` on other threads may run, so a token whose restore is
    /// unsafe then (the env's `setenv`) restores nothing, and the rollback
    /// reports the change it leaves (R164).
    pub(crate) fn threads_may_start(&mut self) {
        self.undo.iter_mut().for_each(Undo::threads_may_start);
    }

    /// The descriptors the tokens hold, which the fd sweep must spare.
    pub(crate) fn held_fds(&self) -> impl Iterator<Item = RawFd> + '_ {
        self.undo.iter().filter_map(Undo::held_fd)
    }

    /// Puts back every recorded change, latest first, and returns what could
    /// not be put back.
    pub(crate) fn roll_back(&mut self) -> Vec<RestoreFailure> {
        std::mem::take(&mut self.undo)
            .into_iter()
            .rev()
            .filter_map(|undo| undo.restore().err())
            .collect()
    }

    /// Keeps every recorded change.
    pub(crate) fn keep_all(&mut self) {
        self.undo.clear();
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        let _ = self.roll_back();
    }
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
        let mut ledger = Ledger::new(Rollback::PutBack);
        set_umask(0o077, &mut ledger).unwrap();
        assert_eq!(ledger.held_fds().count(), 0);
    }

    #[test]
    fn a_token_holding_a_descriptor_spares_it() {
        if !isolated() {
            return;
        }
        let mut ledger = Ledger::new(Rollback::PutBack);
        change_dir(std::path::Path::new("/"), &mut ledger).unwrap();
        assert_eq!(ledger.held_fds().count(), 1);
    }

    #[test]
    fn a_dropped_ledger_restores_latest_first() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(CALLER_UMASK));
        let mut ledger = Ledger::new(Rollback::PutBack);
        set_umask(0o077, &mut ledger).unwrap();
        set_umask(0o027, &mut ledger).unwrap();
        drop(ledger);
        assert_eq!(current_umask().bits(), CALLER_UMASK);
    }

    #[test]
    fn a_kept_ledger_restores_nothing() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(CALLER_UMASK));
        let mut ledger = Ledger::new(Rollback::PutBack);
        set_umask(0o077, &mut ledger).unwrap();
        ledger.keep_all();
        drop(ledger);
        assert_eq!(current_umask().bits(), 0o077);
    }

    #[test]
    fn under_skip_nothing_is_saved() {
        let _ = Undo::saving(Rollback::Skip, || unreachable!("Skip must not save"));
    }

    #[test]
    fn under_skip_nothing_is_restored() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(CALLER_UMASK));
        let mut ledger = Ledger::new(Rollback::Skip);
        set_umask(0o077, &mut ledger).unwrap();
        drop(ledger);
        assert_eq!(current_umask().bits(), 0o077);
    }

    #[test]
    fn a_single_threaded_token_restores_nothing_once_threads_may_start() {
        if !isolated() {
            return;
        }
        let key = "BLIVET_UNDO_TEST";
        let mut ledger = Ledger::new(Rollback::PutBack);
        set_env_vars(&[(key.into(), "set".into())], &mut ledger).unwrap();
        ledger.threads_may_start();
        drop(ledger);
        assert_eq!(std::env::var(key).as_deref(), Ok("set"));
    }

    #[test]
    fn an_env_left_once_threads_may_start_is_reported() {
        if !isolated() {
            return;
        }
        let mut ledger = Ledger::new(Rollback::PutBack);
        set_env_vars(&[("BLIVET_UNDO_TEST".into(), "set".into())], &mut ledger).unwrap();
        ledger.threads_may_start();

        let failures = ledger.roll_back();

        let states: Vec<_> = failures.iter().map(RestoreFailure::state).collect();
        assert_eq!(states, [crate::ChangedState::Environment]);
    }

    #[test]
    fn other_tokens_still_restore_once_threads_may_start() {
        if !isolated() {
            return;
        }
        umask(Mode::from_bits_truncate(CALLER_UMASK));
        let mut ledger = Ledger::new(Rollback::PutBack);
        set_umask(0o077, &mut ledger).unwrap();
        ledger.threads_may_start();
        drop(ledger);
        assert_eq!(current_umask().bits(), CALLER_UMASK);
    }
}

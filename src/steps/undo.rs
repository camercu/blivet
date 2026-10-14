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
    use std::cell::Cell;
    use std::rc::Rc;

    struct Count(Rc<Cell<u32>>);

    impl Restore for Count {
        fn restore(self: Box<Self>) {
            self.0.set(self.0.get() + 1);
        }
    }

    fn counter() -> (Rc<Cell<u32>>, Count) {
        let n = Rc::new(Cell::new(0));
        (n.clone(), Count(n))
    }

    #[test]
    fn a_token_holding_no_descriptor_spares_none() {
        let (_, count) = counter();
        let undo = Undo::new(count);
        assert_eq!(undo.held_fd(), None);
        undo.keep();
    }

    #[test]
    fn a_dropped_token_restores() {
        let (n, count) = counter();
        drop(Undo::new(count));
        assert_eq!(n.get(), 1);
    }

    #[test]
    fn a_kept_token_restores_nothing() {
        let (n, count) = counter();
        Undo::new(count).keep();
        assert_eq!(n.get(), 0);
    }

    #[test]
    fn under_skip_nothing_is_saved_or_restored() {
        let (n, count) = counter();
        let saved = Cell::new(false);
        drop(Undo::saving(Rollback::Skip, || {
            saved.set(true);
            count
        }));
        assert_eq!((saved.get(), n.get()), (false, 0));
    }

    #[test]
    fn under_put_back_the_saved_state_is_restored() {
        let (n, count) = counter();
        drop(Undo::saving(Rollback::PutBack, || count));
        assert_eq!(n.get(), 1);
    }

    #[test]
    fn a_single_threaded_token_restores_nothing_once_threads_may_start() {
        let (n, count) = counter();
        let mut undo = Undo::new(count).single_threaded();
        undo.threads_may_start();
        drop(undo);
        assert_eq!(n.get(), 0);
    }

    #[test]
    fn other_tokens_still_restore_once_threads_may_start() {
        let (n, count) = counter();
        let mut undo = Undo::new(count);
        undo.threads_may_start();
        drop(undo);
        assert_eq!(n.get(), 1);
    }
}

// SPDX-License-Identifier: MIT

//! How the preview is presented, which decides the detail its loads ask for,
//! and who is told when that changes.

use super::*;

#[derive(Default)]
pub(super) struct Listeners(RefCell<Vec<Rc<dyn Fn()>>>);

impl PreviewState {
    /// The detail loads ask for. Standard until something presents the preview
    /// larger than the drawer.
    pub(super) fn current_detail(&self) -> PreviewDetail {
        PreviewDetail::Standard
    }

    /// Runs `listener` every time a change in how the preview is presented has
    /// settled, in the order listeners were added.
    pub(super) fn on_presentation_changed(&self, listener: impl Fn() + 'static) {
        self.presentation.0.borrow_mut().push(Rc::new(listener));
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "called by features that present the preview differently"
        )
    )]
    pub(super) fn presentation_changed(&self) {
        let listeners = self.presentation.0.borrow().clone();
        for listener in listeners {
            listener();
        }
    }
}

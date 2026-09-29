// SPDX-License-Identifier: MIT

//! Who is told when the way the preview is presented has changed.

use super::*;

#[derive(Default)]
pub(super) struct Listeners(RefCell<Vec<Rc<dyn Fn()>>>);

impl PreviewState {
    /// Runs `listener` every time a change in how the preview is presented has
    /// settled, in the order listeners were added.
    pub(super) fn on_presentation_changed(&self, listener: impl Fn() + 'static) {
        self.presentation.0.borrow_mut().push(Rc::new(listener));
    }

    pub(super) fn presentation_changed(&self) {
        let listeners = self.presentation.0.borrow().clone();
        for listener in listeners {
            listener();
        }
    }
}

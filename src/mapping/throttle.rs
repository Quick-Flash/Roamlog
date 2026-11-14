//! Rate-limits work driven by every drag frame without losing the last change.
use std::time::Duration;
use zgui::{prelude::*, reactive::Owner, view::IntervalHandle};

/// Runs work at once, then at most once per interval while changes keep
/// arriving; the final change always runs.
#[derive(Clone, Copy)]
pub struct Throttle {
    interval: Duration,
    active: StoredValue<Option<IntervalHandle>, LocalStorage>,
    dirty: StoredValue<bool, LocalStorage>,
    owner: StoredValue<Owner, LocalStorage>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            active: StoredValue::new_local(None),
            dirty: StoredValue::new_local(false),
            owner: StoredValue::new_local(Owner::current().expect("throttle needs a view owner")),
        }
    }
    pub fn cancel(self) {
        self.active.set_value(None);
        self.dirty.set_value(false);
    }
    pub fn run(self, work: impl Fn() + Copy + 'static) {
        if self.active.with_value(Option::is_some) {
            self.dirty.set_value(true);
            return;
        }
        self.owner.with_value(|owner| owner.with(work));
        // Without a window clock, run every change.
        let Some(timers) = self.owner.with_value(|owner| owner.with(Timers::current)) else {
            return;
        };
        // A repeating timer that cancels itself once idle: a fired one-shot
        // handle would leave its id behind in the timer heap's cancel set.
        let interval = timers.set_interval(self.interval, move || {
            if self.dirty.with_value(|dirty| *dirty) {
                self.dirty.set_value(false);
                // Timer dispatch has no component owner. Restore it so tasks
                // spawned by delayed work are cancelled when this view closes.
                self.owner.with_value(|owner| owner.with(work));
            } else {
                self.active.set_value(None);
            }
        });
        self.active.set_value(Some(interval));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zgui::reactive::Mounted;

    #[test]
    fn delayed_work_tasks_are_cancelled_when_view_unmounts() {
        let window = zgui_testkit_view::Window::open();
        let tasks = window.scope.with(|| StoredValue::new_local(Vec::new()));
        let view = window.scope.with(Mounted::new);
        view.with(|| {
            let throttle = Throttle::new(Duration::from_millis(10));
            on_cleanup_local(move || throttle.cancel());
            let work = move || {
                let task = spawn_local(std::future::pending::<()>());
                tasks.update_value(|tasks| tasks.push(task));
            };
            throttle.run(work);
            throttle.run(work);
        });
        window.advance(Duration::from_millis(10));
        window.frame();
        assert_eq!(tasks.with_value(Vec::len), 2);
        assert!(tasks.with_value(|tasks| tasks.iter().all(|task| !task.is_finished())));
        view.unmount();
        assert!(tasks.with_value(|tasks| tasks.iter().all(|task| task.is_finished())));
        window.advance(Duration::from_millis(20));
        window.frame();
        assert_eq!(tasks.with_value(Vec::len), 2, "timer stopped with view");
    }
}

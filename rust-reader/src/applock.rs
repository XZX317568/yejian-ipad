//! 应用锁 state machine. Pure logic — the platform supplies authentication
//! results and foreground events; the UI polls this every frame. The lock
//! itself lives in memory only: `enabled`/`delay` persist as settings while
//! `locked` resets on every cold start.

/// Delay choices surfaced by the settings picker.
pub const DELAY_CHOICES: &[(u64, &str)] = &[(0, "立即"), (60, "1 分钟"), (300, "5 分钟")];

#[derive(Debug, Default)]
pub struct AppLock {
    enabled: bool,
    delay_secs: u64,
    locked: bool,
    /// Wall-clock second of the last real backgrounding.
    backgrounded_at: Option<u64>,
    /// An authenticate call is in flight; results arrive via
    /// [`AppLock::auth_finished`].
    auth_in_flight: bool,
    /// The lock screen auto-launches one prompt per lock episode.
    auto_attempted: bool,
    /// A settings toggle waiting on a successful auth (`Some(on)`).
    pending_toggle: Option<bool>,
    /// Consumed once by the UI to persist a finished toggle.
    toggle_applied: Option<bool>,
    /// Last auth failure/cancel, shown under the 解锁 button.
    failure: Option<String>,
}

impl AppLock {
    /// Settings restore: adopt the persisted switches without locking (that
    /// is what [`AppLock::cold_start`] is for).
    pub fn apply_settings(&mut self, enabled: bool, delay_secs: u64) {
        self.enabled = enabled;
        self.delay_secs = delay_secs;
    }

    /// Called once after the first settings restore, before any library
    /// content may be painted: an enabled app starts locked.
    pub fn cold_start(&mut self) {
        if self.enabled {
            self.lock();
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn delay_secs(&self) -> u64 {
        self.delay_secs
    }

    /// Lock delay changes apply immediately and never need auth.
    pub fn set_delay(&mut self, delay_secs: u64) {
        self.delay_secs = delay_secs;
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn auth_in_flight(&self) -> bool {
        self.auth_in_flight
    }

    /// Failure/cancel text shown under the 解锁 button until it succeeds.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn lock(&mut self) {
        self.locked = true;
        self.auto_attempted = false;
        self.failure = None;
        self.backgrounded_at = None;
    }

    /// Shell reported the app leaving the foreground.
    pub fn background(&mut self, now: u64) {
        self.backgrounded_at = Some(now);
    }

    /// Shell reported the app coming back: re-lock once the configured
    /// delay elapsed (0 = immediately).
    pub fn foreground(&mut self, now: u64) {
        let Some(at) = self.backgrounded_at.take() else {
            return;
        };
        if self.enabled && now.saturating_sub(at) >= self.delay_secs {
            self.lock();
        }
    }

    /// True exactly once per lock episode: the lock screen auto-starts one
    /// authentication when it first appears.
    pub fn take_auto_attempt(&mut self) -> bool {
        self.locked && !self.auth_in_flight && !std::mem::replace(&mut self.auto_attempted, true)
    }

    /// The 解锁 button: one prompt at a time.
    /// Returns true when the UI should call `authenticate`.
    pub fn begin_unlock(&mut self) -> bool {
        if self.auth_in_flight {
            return false;
        }
        self.auth_in_flight = true;
        true
    }

    /// Changing the switch needs proof of identity in both directions.
    /// Returns true when the UI should call `authenticate`.
    pub fn request_toggle(&mut self, target: bool) -> bool {
        if target == self.enabled || self.auth_in_flight {
            return false;
        }
        self.auth_in_flight = true;
        self.pending_toggle = Some(target);
        true
    }

    /// Consume the "a toggle just took effect" flag so the UI can persist it.
    pub fn take_toggle_applied(&mut self) -> Option<bool> {
        self.toggle_applied.take()
    }

    /// An auth attempt could not even start (unsupported/cancelled by the
    /// system before showing a prompt).
    pub fn auth_failed_to_start(&mut self, error: String) {
        self.auth_finished(Err(error));
    }

    /// Result from `yejian_platform::poll_auth`.
    pub fn auth_finished(&mut self, result: Result<(), String>) {
        self.auth_in_flight = false;
        match result {
            Ok(()) => {
                if let Some(target) = self.pending_toggle.take() {
                    self.enabled = target;
                    self.toggle_applied = Some(target);
                }
                self.locked = false;
                self.failure = None;
            }
            Err(error) => {
                self.pending_toggle = None;
                if self.locked {
                    // Stay locked; the button retries.
                    self.failure = Some("未能解锁，请重试".into());
                } else {
                    self.failure = Some(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_start_locks_only_when_enabled() {
        let mut off = AppLock::default();
        off.apply_settings(false, 0);
        off.cold_start();
        assert!(!off.locked());
        let mut on = AppLock::default();
        on.apply_settings(true, 0);
        on.cold_start();
        assert!(on.locked());
    }

    #[test]
    fn foreground_relocks_once_the_delay_elapsed() {
        let mut lock = AppLock::default();
        lock.apply_settings(true, 60);
        lock.background(100);
        lock.foreground(159);
        assert!(!lock.locked(), "59 seconds is below the 1 minute delay");
        lock.background(100);
        lock.foreground(160);
        assert!(lock.locked());
        // Disabled works never re-lock.
        let mut off = AppLock::default();
        off.background(0);
        off.foreground(10_000);
        assert!(!off.locked());
        // A Foreground without a Background does nothing.
        let mut solo = AppLock::default();
        solo.apply_settings(true, 0);
        solo.foreground(500);
        assert!(!solo.locked());
    }

    #[test]
    fn unlock_succeeds_and_failure_stays_locked() {
        let mut lock = AppLock::default();
        lock.apply_settings(true, 0);
        lock.cold_start();
        assert!(lock.take_auto_attempt(), "the screen auto-prompts once");
        assert!(!lock.take_auto_attempt());
        assert!(lock.begin_unlock());
        assert!(!lock.begin_unlock(), "one prompt at a time");
        lock.auth_finished(Err("cancel".into()));
        assert!(lock.locked());
        assert_eq!(lock.failure(), Some("未能解锁，请重试"));
        assert!(lock.begin_unlock());
        lock.auth_finished(Ok(()));
        assert!(!lock.locked());
        assert!(lock.failure().is_none());
    }

    #[test]
    fn toggling_requires_a_successful_auth() {
        let mut lock = AppLock::default();
        // Failed auth leaves the setting unchanged.
        assert!(lock.request_toggle(true));
        lock.auth_finished(Err("denied".into()));
        assert!(!lock.enabled());
        assert!(lock.take_toggle_applied().is_none());
        // Successful auth applies it.
        assert!(lock.request_toggle(true));
        lock.auth_finished(Ok(()));
        assert!(lock.enabled());
        assert_eq!(lock.take_toggle_applied(), Some(true));
        // Turning it off also requires auth.
        assert!(lock.request_toggle(false));
        lock.auth_finished(Ok(()));
        assert!(!lock.enabled());
        assert_eq!(lock.take_toggle_applied(), Some(false));
        // Requesting the current value never prompts.
        assert!(!lock.request_toggle(false));
    }
}

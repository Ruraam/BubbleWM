//! Idle, screen blanking and suspend policy (replaces swayidle).

use std::time::{Duration, Instant};

use crate::config::IdleConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleAction {
    None,
    Blank,
    Unblank,
    Lock,
    Suspend,
}

#[derive(Debug)]
pub struct IdleManager {
    last_activity: Instant,
    blanked: bool,
    locked_for_idle: bool,
}

impl IdleManager {
    pub fn new() -> Self {
        Self {
            last_activity: Instant::now(),
            blanked: false,
            locked_for_idle: false,
        }
    }

    pub fn activity(&mut self) -> IdleAction {
        self.last_activity = Instant::now();
        self.locked_for_idle = false;
        if self.blanked {
            self.blanked = false;
            IdleAction::Unblank
        } else {
            IdleAction::None
        }
    }

    pub fn blanked(&self) -> bool {
        self.blanked
    }

    /// Evaluates timers. `inhibited` prevents blanking and locking, but the
    /// activity timestamp is still advanced so leaving fullscreen does not
    /// immediately blank the output.
    pub fn tick(&mut self, config: &IdleConfig, inhibited: bool) -> IdleAction {
        if !config.enabled {
            return IdleAction::None;
        }
        let elapsed = self.last_activity.elapsed();
        if inhibited && config.inhibit_when_fullscreen {
            self.last_activity = Instant::now();
            return IdleAction::None;
        }
        if config.suspend_seconds > 0
            && elapsed >= Duration::from_secs(config.suspend_seconds as u64)
        {
            self.last_activity = Instant::now();
            return IdleAction::Suspend;
        }
        if config.lock_seconds > 0
            && elapsed >= Duration::from_secs(config.lock_seconds as u64)
            && !self.locked_for_idle
        {
            self.locked_for_idle = true;
            return IdleAction::Lock;
        }
        if config.dpms_seconds > 0
            && elapsed >= Duration::from_secs(config.dpms_seconds as u64)
            && !self.blanked
        {
            self.blanked = true;
            return IdleAction::Blank;
        }
        IdleAction::None
    }

    pub fn reset_after_unlock(&mut self) {
        self.last_activity = Instant::now();
        self.blanked = false;
    }
}

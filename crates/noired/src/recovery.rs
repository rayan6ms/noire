//! Deterministic bounded recovery scheduling for native audio resources.

/// First retry delay after an unsuccessful immediate recovery attempt.
pub const INITIAL_BACKOFF_MS: u64 = 25;
/// Maximum delay between recovery attempts.
pub const MAX_BACKOFF_MS: u64 = 1_000;

/// Fault class that invalidated the current native graph generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryFault {
    /// The `PipeWire` core disconnected or reported a fatal error.
    Core,
    /// The physical capture stream stopped or rejected its format.
    CaptureStream,
    /// The virtual source stream stopped or rejected its format.
    SourceStream,
    /// The selected physical input disappeared.
    InputUnavailable,
    /// The session default changed and follow-default intent must be resolved again.
    DefaultChanged,
}

/// Low-rate recovery lifecycle owned by the native control thread.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecoveryPhase {
    /// The intended graph is currently realized.
    #[default]
    Healthy,
    /// Resources are absent and the next attempt is time-bounded.
    Backoff,
    /// One reconstruction attempt is in progress on the owner thread.
    Attempting,
}

/// One due reconstruction attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryAttempt {
    /// Monotonic graph generation invalidating all earlier audio.
    pub generation: u64,
    /// One-based attempt number for this fault episode.
    pub number: u32,
    /// Fault that initiated the episode.
    pub fault: RecoveryFault,
}

/// Fixed-cardinality counters suitable for diagnostics and soak assertions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryStats {
    /// Fault episodes observed.
    pub faults: u64,
    /// Reconstruction attempts started.
    pub attempts: u64,
    /// Episodes that returned to healthy operation.
    pub recoveries: u64,
    /// Failed reconstruction attempts.
    pub failures: u64,
}

/// Pure capped-exponential recovery controller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryController {
    phase: RecoveryPhase,
    fault: Option<RecoveryFault>,
    generation: u64,
    attempt: u32,
    due_millis: u64,
    stats: RecoveryStats,
}

impl RecoveryController {
    /// Marks the current resources invalid and schedules an immediate attempt.
    ///
    /// Repeated reports during the same episode do not postpone the existing attempt.
    pub fn fault(&mut self, fault: RecoveryFault, now_millis: u64) {
        if self.phase != RecoveryPhase::Healthy {
            if fault == RecoveryFault::Core && self.fault != Some(RecoveryFault::Core) {
                self.fault = Some(RecoveryFault::Core);
                self.due_millis = self.due_millis.min(now_millis);
            }
            return;
        }
        self.phase = RecoveryPhase::Backoff;
        self.fault = Some(fault);
        self.generation = self.generation.saturating_add(1);
        self.attempt = 0;
        self.due_millis = now_millis;
        self.stats.faults = self.stats.faults.saturating_add(1);
    }

    /// Starts the next attempt if its deadline has arrived.
    #[must_use]
    pub fn poll(&mut self, now_millis: u64) -> Option<RecoveryAttempt> {
        if self.phase != RecoveryPhase::Backoff || now_millis < self.due_millis {
            return None;
        }
        let fault = self.fault?;
        self.phase = RecoveryPhase::Attempting;
        self.attempt = self.attempt.saturating_add(1);
        self.stats.attempts = self.stats.attempts.saturating_add(1);
        Some(RecoveryAttempt {
            generation: self.generation,
            number: self.attempt,
            fault,
        })
    }

    /// Records a failed attempt and schedules the next capped delay.
    pub fn failed(&mut self, now_millis: u64) {
        if self.phase != RecoveryPhase::Attempting {
            return;
        }
        let exponent = self.attempt.saturating_sub(1).min(6);
        let delay = INITIAL_BACKOFF_MS
            .saturating_mul(1_u64 << exponent)
            .min(MAX_BACKOFF_MS);
        self.phase = RecoveryPhase::Backoff;
        self.due_millis = now_millis.saturating_add(delay);
        self.stats.failures = self.stats.failures.saturating_add(1);
    }

    /// Marks the current generation healthy and resets episode-local scheduling.
    pub fn recovered(&mut self) {
        if self.phase == RecoveryPhase::Healthy {
            return;
        }
        self.phase = RecoveryPhase::Healthy;
        self.fault = None;
        self.attempt = 0;
        self.due_millis = 0;
        self.stats.recoveries = self.stats.recoveries.saturating_add(1);
    }

    /// Cancels recovery because processing intent was stopped.
    pub fn stop(&mut self) {
        self.phase = RecoveryPhase::Healthy;
        self.fault = None;
        self.attempt = 0;
        self.due_millis = 0;
    }

    /// Current low-rate phase.
    #[must_use]
    pub const fn phase(&self) -> RecoveryPhase {
        self.phase
    }

    /// Current graph generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Next attempt deadline on the caller's monotonic millisecond clock.
    #[must_use]
    pub const fn due_millis(&self) -> u64 {
        self.due_millis
    }

    /// Fault responsible for the active recovery episode.
    #[must_use]
    pub const fn fault_kind(&self) -> Option<RecoveryFault> {
        self.fault
    }

    /// Fixed-cardinality cumulative counters.
    #[must_use]
    pub const fn stats(&self) -> RecoveryStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn capped_backoff_never_postpones_a_reported_episode() -> Result<(), &'static str> {
        let mut recovery = RecoveryController::default();
        recovery.fault(RecoveryFault::Core, 10);
        recovery.fault(RecoveryFault::InputUnavailable, 20);
        assert_eq!(recovery.due_millis(), 10);
        for expected in 1..=10 {
            let due = recovery.due_millis();
            let attempt = recovery.poll(due).ok_or("attempt must be due")?;
            assert_eq!(attempt.number, expected);
            recovery.failed(due);
            assert!(recovery.due_millis().saturating_sub(due) <= MAX_BACKOFF_MS);
        }
        recovery.recovered();
        assert_eq!(recovery.phase(), RecoveryPhase::Healthy);
        assert_eq!(recovery.stats().recoveries, 1);
        Ok(())
    }

    #[test]
    fn core_loss_supersedes_a_transient_input_fault_without_starting_a_new_episode() {
        let mut recovery = RecoveryController::default();
        recovery.fault(RecoveryFault::InputUnavailable, 100);
        assert!(recovery.poll(100).is_some());
        recovery.failed(100);
        assert_eq!(recovery.due_millis(), 125);

        recovery.fault(RecoveryFault::Core, 110);

        assert_eq!(recovery.fault_kind(), Some(RecoveryFault::Core));
        assert_eq!(recovery.due_millis(), 110);
        assert_eq!(recovery.stats().faults, 1);
        assert_eq!(
            recovery.poll(110).map(|attempt| attempt.fault),
            Some(RecoveryFault::Core)
        );
    }

    #[test]
    fn one_hundred_fault_cycles_advance_generation_without_sticking() {
        let mut recovery = RecoveryController::default();
        for cycle in 0_u64..100 {
            recovery.fault(RecoveryFault::InputUnavailable, cycle);
            assert!(recovery.poll(cycle).is_some());
            recovery.recovered();
        }
        assert_eq!(recovery.generation(), 100);
        assert_eq!(recovery.stats().faults, 100);
        assert_eq!(recovery.stats().recoveries, 100);
    }

    #[test]
    fn default_and_format_changes_create_fresh_graph_generations() -> Result<(), &'static str> {
        let mut recovery = RecoveryController::default();

        recovery.fault(RecoveryFault::DefaultChanged, 100);
        let default_attempt = recovery.poll(100).ok_or("default retry must be due")?;
        assert_eq!(default_attempt.fault, RecoveryFault::DefaultChanged);
        assert_eq!(default_attempt.generation, 1);
        recovery.recovered();

        recovery.fault(RecoveryFault::CaptureStream, 200);
        let format_attempt = recovery.poll(200).ok_or("format retry must be due")?;
        assert_eq!(format_attempt.fault, RecoveryFault::CaptureStream);
        assert_eq!(format_attempt.generation, 2);
        recovery.recovered();

        assert_eq!(recovery.stats().recoveries, 2);
        Ok(())
    }

    proptest! {
        #[test]
        fn arbitrary_fault_and_clock_sequences_remain_bounded(
            events in prop::collection::vec((0_u8..5, 0_u16..2_000), 0..4_096)
        ) {
            let mut recovery = RecoveryController::default();
            let mut now = 0_u64;
            for (event, advance) in events {
                now = now.saturating_add(u64::from(advance));
                match event {
                    0 => recovery.fault(RecoveryFault::Core, now),
                    1 => {
                        if recovery.poll(now).is_some() {
                            recovery.failed(now);
                        }
                    }
                    2 => recovery.recovered(),
                    3 => recovery.stop(),
                    _ => { let _ = recovery.poll(now); }
                }
                prop_assert!(recovery.due_millis().saturating_sub(now) <= MAX_BACKOFF_MS);
                prop_assert!(recovery.stats().recoveries <= recovery.stats().faults);
                prop_assert!(recovery.stats().failures <= recovery.stats().attempts);
            }
        }
    }
}

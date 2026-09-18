use std::time::{Duration, Instant};

pub const LOCAL: Duration = Duration::from_secs(3);
pub const PROBE: Duration = Duration::from_millis(1_500);
pub const INIT: Duration = Duration::from_secs(3);
pub const STARTUP: Duration = Duration::from_secs(6);
// A verified eth_call includes the execution result and the proof work needed to check it.
// The old three-second cap routinely expired on a Multicall3 containing a few ERC-20
// balanceOf legs even though the verified proxy was healthy and still advancing. Keep the
// hop bounded, but give proof-backed portfolio reads a realistic network allowance.
pub const RPC: Duration = Duration::from_secs(12);
pub const READ: Duration = Duration::from_secs(14);
pub const TRANSFER: Duration = Duration::from_millis(4_500);
const MIN: Duration = Duration::from_millis(50);
const MARGIN: Duration = Duration::from_millis(300);

pub struct Budget {
    start: Instant,
    total: Duration,
}

impl Budget {
    pub fn new(total: Duration) -> Self {
        Self {
            start: Instant::now(),
            total,
        }
    }
    pub fn take(&self, cap: Duration) -> Option<Duration> {
        let grant = self.total.checked_sub(self.start.elapsed())?.min(cap);
        (grant >= MIN).then_some(grant)
    }
}

pub fn deadline(transport: Duration) -> Option<i64> {
    transport
        .checked_sub(MARGIN)
        .filter(|d| *d >= MIN)
        .map(|d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_leaves_room_for_the_reply() {
        assert!(deadline(RPC).unwrap() < RPC.as_millis() as i64);
    }

    #[test]
    fn a_verified_multicall_gets_a_working_deadline() {
        // deadline() reserves 300ms for the structured refusal to travel back through
        // eth_rpc. What remains must still cover proof generation for a multi-token read.
        assert!(deadline(RPC).unwrap() >= 11_000);
        assert!(RPC < READ);
    }
    #[test]
    fn a_budget_never_grants_more_than_its_total() {
        assert!(Budget::new(READ).take(Duration::from_secs(99)).unwrap() <= READ);
    }

    #[test]
    fn startup_is_bounded_across_the_probe_and_init() {
        let budget = Budget::new(STARTUP);
        assert_eq!(budget.take(PROBE), Some(PROBE));
        assert!(budget.take(INIT).unwrap() <= INIT);
    }
}

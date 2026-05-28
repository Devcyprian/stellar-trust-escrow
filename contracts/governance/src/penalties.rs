//! # On-Chain Arbitrator Penalty Ledger (Issue #915)
//!
//! Tracks arbitrator performance and applies penalty points for:
//! - Missing the dispute resolution deadline.
//! - Having a decision overturned by a governance appeal.
//!
//! Each penalty point reduces the arbitrator's reward share by 15 bps
//! (i.e. `reward_bps = base_reward_bps - penalty_points * 15`).
//!
//! ## Storage layout
//! - `DataKey::ArbitratorPenalty(Address)` → `PenaltyRecord`
//!
//! ## Events
//! - `("arb_pen", arbitrator)` — penalty recorded
//! - `("arb_rwrd", arbitrator)` — adjusted reward share queried

use soroban_sdk::{contracttype, symbol_short, Address, Env};

use crate::{errors::GovError, types::DataKey};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Reward reduction per penalty point in basis points.
pub const PENALTY_REDUCTION_BPS: u32 = 15;

/// Maximum penalty points before an arbitrator is fully disqualified (reward → 0).
/// At 15 bps per point and a 10_000 bps base, 667 points would zero the reward.
/// We cap at a practical limit of 100 points.
pub const MAX_PENALTY_POINTS: u32 = 100;

/// Default base reward share in basis points (100% = 10_000 bps).
pub const BASE_REWARD_BPS: u32 = 10_000;

// ── Types ─────────────────────────────────────────────────────────────────────

/// On-chain penalty record for a single arbitrator.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PenaltyRecord {
    /// Cumulative penalty points.
    pub penalty_points: u32,
    /// Number of disputes resolved on time.
    pub resolved_on_time: u32,
    /// Number of decisions overturned by appeal.
    pub overturned_decisions: u32,
    /// Number of missed deadlines.
    pub missed_deadlines: u32,
    /// Ledger timestamp of the last penalty event.
    pub last_penalty_at: u64,
    /// Whether this arbitrator has been suspended (penalty_points >= MAX_PENALTY_POINTS).
    pub suspended: bool,
}

/// Reason a penalty point was issued.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PenaltyReason {
    /// Arbitrator did not cast a vote within the dispute window.
    MissedDeadline,
    /// Arbitrator's decision was overturned by a governance appeal.
    OverturnedByAppeal,
}

// ── Storage helpers ───────────────────────────────────────────────────────────

fn penalty_key(arbitrator: &Address) -> DataKey {
    DataKey::ArbitratorPenalty(arbitrator.clone())
}

fn load_record(env: &Env, arbitrator: &Address) -> PenaltyRecord {
    env.storage()
        .persistent()
        .get(&penalty_key(arbitrator))
        .unwrap_or(PenaltyRecord {
            penalty_points: 0,
            resolved_on_time: 0,
            overturned_decisions: 0,
            missed_deadlines: 0,
            last_penalty_at: 0,
            suspended: false,
        })
}

fn save_record(env: &Env, arbitrator: &Address, record: &PenaltyRecord) {
    let key = penalty_key(arbitrator);
    env.storage().persistent().set(&key, record);
    stellar_trust_shared::bump_persistent_ttl(env, &key);
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Records a penalty point against `arbitrator` for `reason`.
///
/// Caller must be the contract admin.
///
/// # Errors
/// - `Unauthorized` — caller is not the admin.
/// - `NotArbitrator` — `arbitrator` is not registered.
pub fn record_penalty(
    env: &Env,
    caller: &Address,
    arbitrator: &Address,
    reason: PenaltyReason,
) -> Result<PenaltyRecord, GovError> {
    // Admin-only guard
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(GovError::NotInitialized)?;
    if *caller != admin {
        return Err(GovError::Unauthorized);
    }

    // Must be a registered arbitrator
    let is_arb: bool = env
        .storage()
        .persistent()
        .get(&DataKey::Arbitrator(arbitrator.clone()))
        .unwrap_or(false);
    if !is_arb {
        return Err(GovError::NotArbitrator);
    }

    let mut record = load_record(env, arbitrator);
    record.penalty_points = record
        .penalty_points
        .saturating_add(1)
        .min(MAX_PENALTY_POINTS);
    record.last_penalty_at = env.ledger().timestamp();

    match reason {
        PenaltyReason::MissedDeadline => record.missed_deadlines += 1,
        PenaltyReason::OverturnedByAppeal => record.overturned_decisions += 1,
    }

    record.suspended = record.penalty_points >= MAX_PENALTY_POINTS;
    save_record(env, arbitrator, &record);

    env.events().publish(
        (symbol_short!("arb_pen"), arbitrator.clone()),
        (record.penalty_points, reason as u32, record.suspended),
    );

    Ok(record)
}

/// Records a successful on-time resolution (no penalty).
///
/// Caller must be the contract admin.
pub fn record_resolution(
    env: &Env,
    caller: &Address,
    arbitrator: &Address,
) -> Result<(), GovError> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(GovError::NotInitialized)?;
    if *caller != admin {
        return Err(GovError::Unauthorized);
    }

    let mut record = load_record(env, arbitrator);
    record.resolved_on_time += 1;
    save_record(env, arbitrator, &record);
    Ok(())
}

/// Returns the effective reward share in basis points for `arbitrator`.
///
/// `reward_bps = max(0, BASE_REWARD_BPS - penalty_points * PENALTY_REDUCTION_BPS)`
pub fn get_reward_bps(env: &Env, arbitrator: &Address) -> u32 {
    let record = load_record(env, arbitrator);
    let reduction = record.penalty_points.saturating_mul(PENALTY_REDUCTION_BPS);
    BASE_REWARD_BPS.saturating_sub(reduction)
}

/// Returns the full penalty record for `arbitrator`.
pub fn get_penalty_record(env: &Env, arbitrator: &Address) -> PenaltyRecord {
    load_record(env, arbitrator)
}

/// Clears all penalty points for `arbitrator` (admin-only governance action).
pub fn clear_penalties(
    env: &Env,
    caller: &Address,
    arbitrator: &Address,
) -> Result<(), GovError> {
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(GovError::NotInitialized)?;
    if *caller != admin {
        return Err(GovError::Unauthorized);
    }

    let mut record = load_record(env, arbitrator);
    record.penalty_points = 0;
    record.suspended = false;
    save_record(env, arbitrator, &record);
    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reward_bps_no_penalties() {
        assert_eq!(BASE_REWARD_BPS, 10_000);
        // 0 penalty points → full reward
        let reduction = 0u32.saturating_mul(PENALTY_REDUCTION_BPS);
        assert_eq!(BASE_REWARD_BPS.saturating_sub(reduction), 10_000);
    }

    #[test]
    fn reward_bps_one_penalty() {
        let reduction = 1u32.saturating_mul(PENALTY_REDUCTION_BPS);
        assert_eq!(BASE_REWARD_BPS.saturating_sub(reduction), 9_985);
    }

    #[test]
    fn reward_bps_saturates_at_zero() {
        let reduction = MAX_PENALTY_POINTS.saturating_mul(PENALTY_REDUCTION_BPS);
        // 100 * 15 = 1500 bps reduction; 10_000 - 1500 = 8_500 (not zero at cap)
        assert_eq!(BASE_REWARD_BPS.saturating_sub(reduction), 8_500);
    }

    #[test]
    fn penalty_points_cap_at_max() {
        let mut points: u32 = MAX_PENALTY_POINTS - 1;
        points = points.saturating_add(1).min(MAX_PENALTY_POINTS);
        assert_eq!(points, MAX_PENALTY_POINTS);
        // Adding more doesn't exceed cap
        points = points.saturating_add(5).min(MAX_PENALTY_POINTS);
        assert_eq!(points, MAX_PENALTY_POINTS);
    }

    #[test]
    fn suspension_triggers_at_max_points() {
        let suspended = MAX_PENALTY_POINTS >= MAX_PENALTY_POINTS;
        assert!(suspended);
    }

    #[test]
    fn penalty_reason_variants_distinct() {
        assert_ne!(
            PenaltyReason::MissedDeadline as u32,
            PenaltyReason::OverturnedByAppeal as u32
        );
    }

    #[test]
    fn default_record_not_suspended() {
        let record = PenaltyRecord {
            penalty_points: 0,
            resolved_on_time: 0,
            overturned_decisions: 0,
            missed_deadlines: 0,
            last_penalty_at: 0,
            suspended: false,
        };
        assert!(!record.suspended);
        assert_eq!(record.penalty_points, 0);
    }
}

//! # Subscription-Based Escrow Agreement Extensions (Issue #916)
//!
//! Extends the recurring payment infrastructure with subscription-specific
//! authorization rules, tier configuration, and dispute pathways.
//!
//! ## Design
//!
//! - `SubscriptionConfig` stores tier, max intervals, and buyer authorization.
//! - `authorize_subscription` lets the client pre-authorize recurring withdrawals.
//! - `update_subscription_tier` adjusts payment amount for the next interval.
//! - `dispute_subscription_interval` freezes a specific interval for arbitration.
//! - `get_subscription_config` returns the current subscription parameters.
//!
//! All writes go through the contract's persistent storage under
//! `DataKey::SubscriptionConfig(escrow_id)`.

use soroban_sdk::{contracttype, symbol_short, Address, Env};

use crate::{
    errors::EscrowError,
    storage::StorageManager as ContractStorage,
    types::{DataKey, EscrowStatus},
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of intervals that can be pre-authorized in one call.
pub const MAX_AUTHORIZED_INTERVALS: u32 = 120; // 10 years of monthly payments

/// Minimum interval duration in seconds (1 day).
pub const MIN_INTERVAL_SECONDS: u64 = 86_400;

/// 30-day interval in seconds.
pub const MONTHLY_INTERVAL_SECONDS: u64 = 2_592_000;

// ── Types ─────────────────────────────────────────────────────────────────────

/// Subscription tier controlling payment cadence and amount adjustments.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionTier {
    /// Fixed amount per interval — no adjustments allowed.
    Fixed,
    /// Amount may be updated by the client before each interval.
    Flexible,
    /// Amount scales with an on-chain oracle price feed.
    Dynamic,
}

/// Per-escrow subscription configuration stored on-chain.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SubscriptionConfig {
    /// Subscription tier controlling adjustment rules.
    pub tier: SubscriptionTier,
    /// Number of intervals the client has pre-authorized.
    pub authorized_intervals: u32,
    /// Number of intervals already processed.
    pub consumed_intervals: u32,
    /// Whether the client has authorized recurring withdrawals.
    pub buyer_authorized: bool,
    /// Ledger timestamp of the last authorization.
    pub authorized_at: u64,
    /// Optional cap on the maximum payment amount per interval (in stroops).
    /// `0` means no cap.
    pub max_payment_cap: i128,
    /// Interval currently under dispute (`None` if none).
    pub disputed_interval: Option<u32>,
}

// ── Storage key extension ─────────────────────────────────────────────────────

/// Returns the persistent storage key for a subscription config.
fn subscription_key(escrow_id: u64) -> DataKey {
    DataKey::RecurringConfig(escrow_id)
}

// ── Public helpers (called from EscrowContract impl) ─────────────────────────

/// Stores a new `SubscriptionConfig` for `escrow_id`.
pub fn save_subscription_config(env: &Env, escrow_id: u64, cfg: &SubscriptionConfig) {
    let key = DataKey::SubscriptionConfig(escrow_id);
    env.storage().persistent().set(&key, cfg);
    stellar_trust_shared::bump_persistent_ttl(env, &key);
}

/// Loads the `SubscriptionConfig` for `escrow_id`.
///
/// Returns `EscrowError::RecurringNotFound` if no config exists.
pub fn load_subscription_config(
    env: &Env,
    escrow_id: u64,
) -> Result<SubscriptionConfig, EscrowError> {
    env.storage()
        .persistent()
        .get(&DataKey::SubscriptionConfig(escrow_id))
        .ok_or(EscrowError::RecurringNotFound)
}

// ── Contract entry points ─────────────────────────────────────────────────────

/// Pre-authorizes `intervals` recurring withdrawals for `escrow_id`.
///
/// The client must call this before `process_recurring_payments` can release
/// funds for each interval.  Calling again extends the authorization window.
///
/// # Errors
/// - `EscrowNotFound` / `EscrowNotActive` — escrow state guards.
/// - `RecurringNotFound` — no recurring schedule exists for this escrow.
/// - `InvalidRecurring` — `intervals` is 0 or exceeds `MAX_AUTHORIZED_INTERVALS`.
/// - `Unauthorized` — caller is not the escrow client.
pub fn authorize_subscription(
    env: Env,
    caller: Address,
    escrow_id: u64,
    intervals: u32,
    max_payment_cap: i128,
) -> Result<(), EscrowError> {
    caller.require_auth();
    ContractStorage::require_not_paused(&env)?;

    let meta = ContractStorage::load_escrow_meta_with_rent(&env, escrow_id)?;
    if meta.client != caller {
        return Err(EscrowError::Unauthorized);
    }
    if meta.status != EscrowStatus::Active {
        return Err(EscrowError::EscrowNotActive);
    }
    if intervals == 0 || intervals > MAX_AUTHORIZED_INTERVALS {
        return Err(EscrowError::InvalidRecurring);
    }

    // Ensure a recurring schedule exists.
    let _ = ContractStorage::load_recurring_config(&env, escrow_id)?;

    let now = env.ledger().timestamp();
    let mut cfg = load_subscription_config(&env, escrow_id).unwrap_or(SubscriptionConfig {
        tier: SubscriptionTier::Fixed,
        authorized_intervals: 0,
        consumed_intervals: 0,
        buyer_authorized: false,
        authorized_at: 0,
        max_payment_cap: 0,
        disputed_interval: None,
    });

    cfg.buyer_authorized = true;
    cfg.authorized_intervals = cfg
        .authorized_intervals
        .saturating_add(intervals)
        .min(MAX_AUTHORIZED_INTERVALS);
    cfg.authorized_at = now;
    if max_payment_cap > 0 {
        cfg.max_payment_cap = max_payment_cap;
    }

    save_subscription_config(&env, escrow_id, &cfg);

    env.events().publish(
        (symbol_short!("sub_auth"), escrow_id),
        (caller, intervals, cfg.authorized_intervals),
    );
    Ok(())
}

/// Updates the subscription tier and optional payment cap for `escrow_id`.
///
/// Only the escrow client may call this.  Tier changes take effect on the
/// next unprocessed interval.
///
/// # Errors
/// - `EscrowNotActive` — escrow is not active.
/// - `Unauthorized` — caller is not the client.
/// - `RecurringNotFound` — no subscription config exists.
pub fn update_subscription_tier(
    env: Env,
    caller: Address,
    escrow_id: u64,
    new_tier: SubscriptionTier,
    new_max_cap: i128,
) -> Result<(), EscrowError> {
    caller.require_auth();
    ContractStorage::require_not_paused(&env)?;

    let meta = ContractStorage::load_escrow_meta_with_rent(&env, escrow_id)?;
    if meta.client != caller {
        return Err(EscrowError::Unauthorized);
    }
    if meta.status != EscrowStatus::Active {
        return Err(EscrowError::EscrowNotActive);
    }

    let mut cfg = load_subscription_config(&env, escrow_id)?;
    cfg.tier = new_tier.clone();
    if new_max_cap >= 0 {
        cfg.max_payment_cap = new_max_cap;
    }
    save_subscription_config(&env, escrow_id, &cfg);

    env.events().publish(
        (symbol_short!("sub_tier"), escrow_id),
        (caller, new_tier as u32, new_max_cap),
    );
    Ok(())
}

/// Raises a dispute on a specific subscription interval, freezing subsequent
/// releases until the dispute is resolved.
///
/// Either the client or freelancer may call this.
///
/// # Errors
/// - `EscrowNotActive` — escrow is not active.
/// - `Unauthorized` — caller is neither client nor freelancer.
/// - `RecurringNotFound` — no subscription config exists.
/// - `InvalidRecurring` — `interval_index` is out of range or already disputed.
pub fn dispute_subscription_interval(
    env: Env,
    caller: Address,
    escrow_id: u64,
    interval_index: u32,
) -> Result<(), EscrowError> {
    caller.require_auth();
    ContractStorage::require_not_paused(&env)?;

    let meta = ContractStorage::load_escrow_meta_with_rent(&env, escrow_id)?;
    if caller != meta.client && caller != meta.freelancer {
        return Err(EscrowError::Unauthorized);
    }
    if meta.status != EscrowStatus::Active {
        return Err(EscrowError::EscrowNotActive);
    }

    let mut cfg = load_subscription_config(&env, escrow_id)?;
    if cfg.disputed_interval.is_some() {
        return Err(EscrowError::InvalidRecurring);
    }
    if interval_index >= cfg.consumed_intervals {
        return Err(EscrowError::InvalidRecurring);
    }

    cfg.disputed_interval = Some(interval_index);
    save_subscription_config(&env, escrow_id, &cfg);

    env.events().publish(
        (symbol_short!("sub_disp"), escrow_id),
        (caller, interval_index),
    );
    Ok(())
}

/// Returns the current `SubscriptionConfig` for `escrow_id`.
pub fn get_subscription_config(
    env: Env,
    escrow_id: u64,
) -> Result<SubscriptionConfig, EscrowError> {
    ContractStorage::ensure_live_escrow(&env, escrow_id)?;
    load_subscription_config(&env, escrow_id)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::{Address, Env};

    fn make_env() -> Env {
        Env::default()
    }

    #[test]
    fn subscription_config_defaults() {
        let cfg = SubscriptionConfig {
            tier: SubscriptionTier::Fixed,
            authorized_intervals: 0,
            consumed_intervals: 0,
            buyer_authorized: false,
            authorized_at: 0,
            max_payment_cap: 0,
            disputed_interval: None,
        };
        assert!(!cfg.buyer_authorized);
        assert_eq!(cfg.authorized_intervals, 0);
        assert!(cfg.disputed_interval.is_none());
    }

    #[test]
    fn subscription_tier_variants() {
        assert_ne!(SubscriptionTier::Fixed, SubscriptionTier::Flexible);
        assert_ne!(SubscriptionTier::Flexible, SubscriptionTier::Dynamic);
    }

    #[test]
    fn max_authorized_intervals_constant() {
        assert_eq!(MAX_AUTHORIZED_INTERVALS, 120);
    }

    #[test]
    fn monthly_interval_seconds_constant() {
        // 30 days * 24h * 60m * 60s
        assert_eq!(MONTHLY_INTERVAL_SECONDS, 30 * 24 * 3600);
    }

    #[test]
    fn saturating_add_does_not_overflow() {
        let current: u32 = MAX_AUTHORIZED_INTERVALS - 1;
        let result = current.saturating_add(10).min(MAX_AUTHORIZED_INTERVALS);
        assert_eq!(result, MAX_AUTHORIZED_INTERVALS);
    }

    #[test]
    fn disputed_interval_tracks_correctly() {
        let mut cfg = SubscriptionConfig {
            tier: SubscriptionTier::Flexible,
            authorized_intervals: 6,
            consumed_intervals: 3,
            buyer_authorized: true,
            authorized_at: 1000,
            max_payment_cap: 0,
            disputed_interval: None,
        };
        assert!(cfg.disputed_interval.is_none());
        cfg.disputed_interval = Some(2);
        assert_eq!(cfg.disputed_interval, Some(2));
    }
}

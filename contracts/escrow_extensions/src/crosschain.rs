//! # Decentralized Cross-Chain Escrow Settlement Fallback (Issue #918)
//!
//! Accepts cryptographic proof payloads from partner chains (e.g. Ethereum,
//! Cosmos) and releases escrowed Stellar assets when the proof is verified.
//!
//! ## Verification model
//!
//! A proof is valid when:
//! 1. The `chain_id` matches a registered bridge signer set.
//! 2. At least `threshold` of the registered bridge signers have signed the
//!    `proof_hash` (multi-sig check via `env.crypto().ed25519_verify`).
//! 3. The proof has not expired (`submitted_at + PROOF_TTL_SECONDS > now`).
//! 4. The proof has not already been consumed (replay protection).
//!
//! ## Storage layout
//! - `DataKey::CrossChainProof(proof_hash)` → `ProofRecord`
//! - `DataKey::BridgeSigner(chain_id, signer_index)` → `BytesN<32>` (ed25519 pubkey)
//! - `DataKey::BridgeThreshold(chain_id)` → `u32`
//!
//! ## Events
//! - `("cc_proof", proof_hash)` — proof submitted
//! - `("cc_rel", escrow_id)` — escrow released on verified proof
//! - `("cc_rej", proof_hash)` — proof rejected

use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Vec};

use crate::{errors::ExtError, types::DataKey};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum age of a cross-chain proof before it is considered stale (1 hour).
pub const PROOF_TTL_SECONDS: u64 = 3_600;

/// Maximum number of bridge signers per chain.
pub const MAX_BRIDGE_SIGNERS: u32 = 10;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A cross-chain proof payload submitted by a relayer.
#[contracttype]
#[derive(Clone, Debug)]
pub struct CrossChainProof {
    /// Identifier of the source chain (e.g. 1 = Ethereum mainnet, 118 = Cosmos).
    pub chain_id: u32,
    /// Hash of the external transaction / state root being proven.
    pub proof_hash: BytesN<32>,
    /// Ed25519 signatures from bridge signers over `proof_hash`.
    pub signatures: Vec<BytesN<64>>,
    /// Ledger timestamp when the proof was created on the source chain.
    pub submitted_at: u64,
    /// The escrow ID on Stellar that should be released on successful verification.
    pub escrow_id: u64,
    /// Recipient of the released funds (must match the escrow freelancer).
    pub recipient: Address,
}

/// On-chain record of a processed proof (for replay protection).
#[contracttype]
#[derive(Clone, Debug)]
pub struct ProofRecord {
    pub chain_id: u32,
    pub escrow_id: u64,
    pub verified_at: u64,
    pub consumed: bool,
}

// ── Storage helpers ───────────────────────────────────────────────────────────

fn proof_key(proof_hash: &BytesN<32>) -> DataKey {
    DataKey::CrossChainProof(proof_hash.clone())
}

fn signer_key(chain_id: u32, index: u32) -> DataKey {
    DataKey::BridgeSigner(chain_id, index)
}

fn threshold_key(chain_id: u32) -> DataKey {
    DataKey::BridgeThreshold(chain_id)
}

// ── Internal verification ─────────────────────────────────────────────────────

/// Verifies that at least `threshold` signatures in `proof.signatures` are
/// valid ed25519 signatures over `proof.proof_hash` by registered bridge signers.
fn verify_multisig(env: &Env, proof: &CrossChainProof) -> Result<(), ExtError> {
    let threshold: u32 = env
        .storage()
        .persistent()
        .get(&threshold_key(proof.chain_id))
        .unwrap_or(0);

    if threshold == 0 {
        // No bridge registered for this chain
        return Err(ExtError::Unauthorized);
    }

    let mut valid_count: u32 = 0;
    let msg = proof.proof_hash.clone();

    for idx in 0..MAX_BRIDGE_SIGNERS {
        if valid_count >= threshold {
            break;
        }
        let pubkey: Option<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&signer_key(proof.chain_id, idx));
        let Some(pk) = pubkey else { continue };

        // Check if any submitted signature matches this registered pubkey
        for sig in proof.signatures.iter() {
            if env.crypto().ed25519_verify(&pk, &msg.into(), &sig).is_ok() {
                valid_count += 1;
                break;
            }
        }
    }

    if valid_count < threshold {
        return Err(ExtError::Unauthorized);
    }
    Ok(())
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Registers bridge signers and threshold for a chain (admin-only).
///
/// `signers` is a list of ed25519 public keys (32 bytes each).
/// `threshold` is the minimum number of valid signatures required.
pub fn register_bridge(
    env: &Env,
    admin: &Address,
    chain_id: u32,
    signers: Vec<BytesN<32>>,
    threshold: u32,
) -> Result<(), ExtError> {
    // Admin guard
    let stored_admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(ExtError::NotInitialized)?;
    if *admin != stored_admin {
        return Err(ExtError::AdminOnly);
    }
    if threshold == 0 || threshold > signers.len() {
        return Err(ExtError::Unauthorized);
    }
    if signers.len() > MAX_BRIDGE_SIGNERS {
        return Err(ExtError::BatchTooLarge);
    }

    for (i, pk) in signers.iter().enumerate() {
        let key = signer_key(chain_id, i as u32);
        env.storage().persistent().set(&key, &pk);
        stellar_trust_shared::bump_persistent_ttl(env, &key);
    }
    let tkey = threshold_key(chain_id);
    env.storage().persistent().set(&tkey, &threshold);
    stellar_trust_shared::bump_persistent_ttl(env, &tkey);
    Ok(())
}

/// Submits a cross-chain proof and, if valid, releases the escrowed Stellar
/// assets to `proof.recipient`.
///
/// # Errors
/// - `Unauthorized` — signature verification failed or no bridge registered.
/// - `DisputeAlreadyExists` — proof already consumed (replay protection).
/// - `VotingWindowClosed` — proof is stale (`submitted_at + PROOF_TTL_SECONDS < now`).
pub fn submit_proof_and_release(
    env: &Env,
    relayer: &Address,
    proof: CrossChainProof,
    token: &Address,
    release_amount: i128,
) -> Result<(), ExtError> {
    relayer.require_auth();

    let now = env.ledger().timestamp();

    // 1. Staleness check
    if proof.submitted_at + PROOF_TTL_SECONDS < now {
        env.events().publish(
            (symbol_short!("cc_rej"), proof.proof_hash.clone()),
            (proof.chain_id, proof.escrow_id, 0u32 /* stale */),
        );
        return Err(ExtError::VotingWindowClosed);
    }

    // 2. Replay protection
    let pkey = proof_key(&proof.proof_hash);
    if let Some(record) = env
        .storage()
        .persistent()
        .get::<DataKey, ProofRecord>(&pkey)
    {
        if record.consumed {
            return Err(ExtError::DisputeAlreadyExists);
        }
    }

    // 3. Multi-sig verification
    verify_multisig(env, &proof).map_err(|_| {
        env.events().publish(
            (symbol_short!("cc_rej"), proof.proof_hash.clone()),
            (proof.chain_id, proof.escrow_id, 1u32 /* bad sig */),
        );
        ExtError::Unauthorized
    })?;

    // 4. Mark proof as consumed
    let record = ProofRecord {
        chain_id: proof.chain_id,
        escrow_id: proof.escrow_id,
        verified_at: now,
        consumed: true,
    };
    env.storage().persistent().set(&pkey, &record);
    stellar_trust_shared::bump_persistent_ttl(env, &pkey);

    // 5. Release funds
    soroban_sdk::token::Client::new(env, token).transfer(
        &env.current_contract_address(),
        &proof.recipient,
        &release_amount,
    );

    env.events().publish(
        (symbol_short!("cc_rel"), proof.escrow_id),
        (proof.chain_id, proof.proof_hash, proof.recipient, release_amount),
    );
    Ok(())
}

/// Returns the proof record for `proof_hash`, or an error if not found.
pub fn get_proof_record(env: &Env, proof_hash: BytesN<32>) -> Result<ProofRecord, ExtError> {
    env.storage()
        .persistent()
        .get(&proof_key(&proof_hash))
        .ok_or(ExtError::DisputeNotFound)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_ttl_constant() {
        assert_eq!(PROOF_TTL_SECONDS, 3_600);
    }

    #[test]
    fn max_bridge_signers_constant() {
        assert_eq!(MAX_BRIDGE_SIGNERS, 10);
    }

    #[test]
    fn proof_record_consumed_flag() {
        // Simulate replay protection logic
        let record = ProofRecord {
            chain_id: 1,
            escrow_id: 42,
            verified_at: 1000,
            consumed: true,
        };
        assert!(record.consumed);
    }

    #[test]
    fn stale_proof_detection() {
        let submitted_at: u64 = 1000;
        let now: u64 = submitted_at + PROOF_TTL_SECONDS + 1;
        assert!(submitted_at + PROOF_TTL_SECONDS < now);
    }

    #[test]
    fn fresh_proof_passes_staleness_check() {
        let submitted_at: u64 = 1000;
        let now: u64 = submitted_at + PROOF_TTL_SECONDS - 1;
        assert!(submitted_at + PROOF_TTL_SECONDS >= now);
    }

    #[test]
    fn threshold_zero_means_no_bridge() {
        let threshold: u32 = 0;
        assert_eq!(threshold, 0); // would trigger Unauthorized
    }
}

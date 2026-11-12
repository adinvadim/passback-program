use crate::math::{add, ahead};
use crate::BoardError;
use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct Config {
    pub authority: Pubkey,
    pub moderator: Pubkey,
    pub admission_signer: Pubkey,
    pub usdc_mint: Pubkey,
    pub treasury: Pubkey,
    pub passback_bps: u16,
    pub referral_bps: u16,
    pub min_first_amount: u64,
    pub min_amount: u64,
    pub max_amount: u64,
    pub top_jump: u64,
    pub paused: bool,
    pub seq: u64,
}
#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace)]
pub struct ConfigSettings {
    pub authority: Pubkey,
    pub moderator: Pubkey,
    pub admission_signer: Pubkey,
    pub treasury: Pubkey,
    pub max_amount: u64,
}
#[account]
#[derive(InitSpace)]
pub struct Project {
    #[max_len(256)]
    pub identity_key: String,
    pub identity_hash: [u8; 32],
    pub category_id: u16,
    pub payout_wallet: Pubkey,
    pub referrer: Option<Pubkey>,
    pub creator: Pubkey,
    pub all_time_amount: u64,
    pub reached_seq: u64,
    pub created_at: i64,
    pub blocked: bool,
}
#[account]
#[derive(InitSpace)]
pub struct DayBoard {
    pub day: u64,
    #[max_len(10)]
    pub top: Vec<TopEntry>,
    pub volume: u64,
    pub payments: u64,
}
#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace)]
pub struct TopEntry {
    pub project: Pubkey,
    pub amount: u64,
    pub reached_seq: u64,
}

// Pure top-10 rules shared by the instruction handlers and the property tests (src/fuzz.rs).
// They take the board's `top` vector only, so they run without accounts or a validator.

/// Maximum number of ranked entries on a `DayBoard`.
pub const TOP_LEN: usize = 10;

/// Inserts or repositions `entry`, keeping `(amount desc, reached_seq asc)` order and at most
/// `TOP_LEN` entries. An entry ranked below a full board is dropped again by the truncation.
pub fn insert_top(top: &mut Vec<TopEntry>, entry: TopEntry) {
    top.retain(|e| e.project != entry.project);
    top.push(entry);
    top.sort_by(|a, b| {
        b.amount
            .cmp(&a.amount)
            .then(a.reached_seq.cmp(&b.reached_seq))
    });
    top.truncate(TOP_LEN);
}

/// Taking Today's #1 from another project requires at least the leader's amount plus `top_jump`.
pub fn check_top_jump(
    top: &[TopEntry],
    project: &Pubkey,
    new_amount: u64,
    top_jump: u64,
) -> Result<()> {
    if let Some(leader) = top.first() {
        if leader.project != *project && new_amount > leader.amount {
            require!(
                new_amount >= add(leader.amount, top_jump)?,
                BoardError::TopJumpTooSmall
            );
        }
    }
    Ok(())
}

/// Board entries a payment passes (spec §3.3): another project ranked above the payer's old
/// `(amount, reached_seq)` and below its new one. `top` is the board before the payment.
///
/// The set and its pro-rata weights are read at execution, not bound by the payer's signature.
/// A passed project that raises its own amount before a pending payment lands takes a larger
/// share of the same pool (review R12, accepted in spec §3.3): raising is a legitimate paid move,
/// and the pool is fixed at `passback_bps` of the signed charge, which `max_amount` caps.
pub fn passed_entries(
    top: &[TopEntry],
    project: &Pubkey,
    old_amount: u64,
    old_seq: u64,
    new_amount: u64,
    new_seq: u64,
) -> Vec<TopEntry> {
    top.iter()
        .filter(|entry| {
            entry.project != *project
                && ahead(entry.amount, entry.reached_seq, old_amount, old_seq)
                && ahead(new_amount, new_seq, entry.amount, entry.reached_seq)
        })
        .cloned()
        .collect()
}

/// `refill_top` rule: an unranked entry enters when the board has a free slot or it ranks above
/// the lowest entry, evicting that entry when the board is full.
pub fn refill_entry(top: &mut Vec<TopEntry>, entry: TopEntry) -> Result<()> {
    require!(
        !top.iter().any(|e| e.project == entry.project),
        BoardError::AlreadyRanked
    );
    require!(
        top.len() < TOP_LEN
            || top.last().is_some_and(|last| ahead(
                entry.amount,
                entry.reached_seq,
                last.amount,
                last.reached_seq
            )),
        BoardError::DoesNotQualify
    );
    insert_top(top, entry);
    Ok(())
}
/// `set_project_status(blocked = true)` repair (spec §6): removes `blocked` and inserts the
/// supplied replacement. A block that frees a slot must fill it with an unranked project, so an
/// already-ranked replacement fails with `AlreadyRanked` instead of leaving a short board. When
/// the project is no longer ranked (a re-sent block), an already-ranked replacement changes
/// nothing, so the repair is idempotent.
pub fn block_entry(
    top: &mut Vec<TopEntry>,
    blocked: &Pubkey,
    replacement: Option<TopEntry>,
) -> Result<()> {
    let len = top.len();
    top.retain(|e| e.project != *blocked);
    let freed = top.len() < len;
    if let Some(entry) = replacement {
        require!(entry.project != *blocked, BoardError::Blocked);
        if top.iter().any(|e| e.project == entry.project) {
            require!(!freed, BoardError::AlreadyRanked);
        } else {
            insert_top(top, entry);
        }
    }
    Ok(())
}

/// Highest on-chain category id. Off-chain ids are the `sortOrder` values of
/// `convex/domain/categories.ts` (10-190 today), so 1-255 leaves room for new categories while a
/// mistyped or out-of-domain u16 never reaches program state or the indexer.
pub const MAX_CATEGORY_ID: u16 = 255;

/// Category ids accepted at creation and by `set_category`.
pub fn check_category(category_id: u16) -> Result<()> {
    require!(
        (1..=MAX_CATEGORY_ID).contains(&category_id),
        BoardError::InvalidCategory
    );
    Ok(())
}
/// Referrer recorded at creation. `Pubkey::default()` means none. Any other key must be an ed25519
/// point: an off-curve key (a PDA) can never sign `claim_referral`, so its 5% share would stay in
/// Vault forever (review F7 residual). The curve check is the `sol_curve_validate_point` syscall
/// on chain and curve25519-dalek on the host.
pub fn creation_referrer(referrer_wallet: Pubkey) -> Result<Option<Pubkey>> {
    if referrer_wallet == Pubkey::default() {
        return Ok(None);
    }
    require!(
        solana_curve25519::edwards::validate_edwards(&solana_curve25519::edwards::PodEdwardsPoint(
            referrer_wallet.to_bytes()
        )),
        BoardError::WrongReferrer
    );
    Ok(Some(referrer_wallet))
}
/// Longest an admission attestation may stay valid past the validator clock, in seconds. Convex
/// issues five-minute attestations; the bound keeps a leaked one, or one replayed from another
/// cluster, from being hoarded (review F18, spec §4.2).
pub const MAX_ADMISSION_TTL: i64 = 3600;

/// An attestation is accepted while `now <= expires_at <= now + MAX_ADMISSION_TTL`. A longer
/// lifetime is not a valid attestation, so it fails like a bad signature (`AdmissionRequired`).
pub fn check_admission_window(expires_at: i64, now: i64) -> Result<()> {
    require!(expires_at >= now, BoardError::AdmissionExpired);
    require!(
        expires_at <= now.saturating_add(MAX_ADMISSION_TTL),
        BoardError::AdmissionRequired
    );
    Ok(())
}
#[account]
#[derive(InitSpace)]
pub struct ProjectDay {
    pub project: Pubkey,
    pub day: u64,
    pub amount: u64,
    pub passback_accrued: u64,
    pub passback_claimed: u64,
    pub reached_seq: u64,
    /// Canonical PDA bump, stored at creation so later reads verify the address with one
    /// `create_program_address` instead of a bump search whose cost the identity's owner can grind.
    pub bump: u8,
}
#[account]
#[derive(InitSpace)]
pub struct Referrer {
    pub wallet: Pubkey,
    pub accrued: u64,
    pub claimed: u64,
}
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct Creation {
    pub identity_key: String,
    pub category_id: u16,
    pub expires_at: i64,
}

#[event]
pub struct ConfigUpdated {
    pub config_state: Config,
}
#[event]
pub struct Paid {
    pub project: Pubkey,
    pub payer: Pubkey,
    pub amount: u64,
    pub treasury_amount: u64,
    pub referral_amount: u64,
    pub passback_amount: u64,
    pub project_state: Project,
    pub project_day_state: ProjectDay,
    pub referrer_state: Referrer,
    pub board_state: DayBoard,
    pub config_state: Config,
}
#[event]
pub struct PassbackCredited {
    pub amount: u64,
    pub project_day_state: ProjectDay,
}
#[event]
pub struct Claimed {
    pub payout_wallet: Pubkey,
    pub amount: u64,
    pub project_day_state: ProjectDay,
}
#[event]
pub struct ReferralClaimed {
    pub amount: u64,
    pub referrer_state: Referrer,
}
#[event]
pub struct TopRefilled {
    pub project: Pubkey,
    pub board_state: DayBoard,
}
#[event]
pub struct ProjectStatusChanged {
    pub project: Pubkey,
    pub project_state: Project,
    pub board_state: DayBoard,
}
#[event]
pub struct CategoryChanged {
    pub project: Pubkey,
    pub project_state: Project,
}
#[event]
pub struct PayoutWalletChanged {
    pub project: Pubkey,
    pub project_state: Project,
}
#[event]
pub struct PausedChanged {
    pub config_state: Config,
}
#[event]
pub struct ProjectDayClosed {
    pub project: Pubkey,
    pub day: u64,
    pub payout_wallet: Pubkey,
}

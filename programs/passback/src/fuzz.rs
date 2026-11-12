//! Property and model-based tests for the pure payment math (`math.rs`) and the Today top-10
//! rules (`state.rs`). See onchain/README.md, "Fuzz and property tests".
//!
//! Runs are deterministic by default: every property uses `DEFAULT_SEED` unless
//! `PROPTEST_RNG_SEED` is set. `PROPTEST_CASES` raises or lowers the case count.

use crate::math::*;
use crate::state::*;
use anchor_lang::prelude::Pubkey;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, RngSeed};
use std::collections::BTreeSet;

const DEFAULT_SEED: u64 = 0x100_15_7e_e5_0;
const DEFAULT_CASES: u32 = 512;
const PASSBACK_BPS: u16 = 3000;
const REFERRAL_BPS: u16 = 500;
const TOP_JUMP: u64 = 5 * DOLLAR;
const MIN_FIRST: u64 = 10 * DOLLAR;

/// proptest reads `PROPTEST_CASES` / `PROPTEST_RNG_SEED` in `Config::default()`; fixed defaults
/// keep the local gate reproducible.
fn config() -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_RNG_SEED").is_none() {
        config.rng_seed = RngSeed::Fixed(DEFAULT_SEED);
    }
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = DEFAULT_CASES;
    }
    config
}

fn key(index: usize) -> Pubkey {
    Pubkey::new_from_array([u8::try_from(index + 1).unwrap(); 32])
}
fn snapshot(top: &[TopEntry]) -> Vec<(Pubkey, u64, u64)> {
    top.iter()
        .map(|e| (e.project, e.amount, e.reached_seq))
        .collect()
}
fn index_of(project: &Pubkey) -> usize {
    usize::from(project.to_bytes()[0]) - 1
}

// ---------------------------------------------------------------------------------------------
// math.rs
// ---------------------------------------------------------------------------------------------

/// Any `(passback_bps, referral_bps)` with a sum the split can honour.
fn valid_bps() -> impl Strategy<Value = (u16, u16)> {
    (0u16..=10_000).prop_flat_map(|passback| (Just(passback), 0u16..=(10_000 - passback)))
}
/// Charges across the whole u64 range, weighted towards realistic whole-dollar amounts and the
/// edges where rounding and overflow live.
fn charge() -> impl Strategy<Value = u64> {
    prop_oneof![
        4 => (1u64..=999_999).prop_map(|dollars| dollars * DOLLAR),
        2 => any::<u64>(),
        1 => 0u64..=10_000,
        1 => (u64::MAX - 10_000)..=u64::MAX,
    ]
}
/// Up to ten board amounts whose sum always fits in u64.
fn passed_amounts() -> impl Strategy<Value = Vec<u64>> {
    prop_oneof![
        3 => prop::collection::vec((1u64..=2_000).prop_map(|dollars| dollars * DOLLAR), 0..=TOP_LEN),
        1 => prop::collection::vec(1u64..=u64::MAX / 16, 0..=TOP_LEN),
        1 => prop::collection::vec(1u64..=3, 0..=TOP_LEN),
    ]
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn checked_arithmetic_matches_u128(a in any::<u64>(), b in any::<u64>()) {
        prop_assert_eq!(add(a, b).ok(), a.checked_add(b));
        prop_assert_eq!(sub(a, b).ok(), a.checked_sub(b));
    }

    #[test]
    fn ratio_is_floor_and_never_panics(amount in any::<u64>(), numerator in any::<u64>(), denominator in any::<u64>()) {
        let exact = (denominator > 0).then(|| u128::from(amount) * u128::from(numerator) / u128::from(denominator));
        let expected = exact.and_then(|value| u64::try_from(value).ok());
        prop_assert_eq!(ratio(amount, numerator, denominator).ok(), expected);
        if denominator > 0 && numerator <= denominator {
            // A share never exceeds its whole, so it can never overflow.
            prop_assert!(ratio(amount, numerator, denominator).unwrap() <= amount);
        }
    }

    #[test]
    fn split_conserves_the_charge(
        charge in charge(),
        (passback_bps, referral_bps) in valid_bps(),
        has_referrer in any::<bool>(),
        amounts in passed_amounts(),
    ) {
        let split = split_payment(charge, passback_bps, referral_bps, has_referrer, &amounts).unwrap();
        let pool = ratio(charge, u64::from(passback_bps), 10_000).unwrap();
        let referral = if has_referrer { ratio(charge, u64::from(referral_bps), 10_000).unwrap() } else { 0 };
        // Three outflows sum exactly to the charge (u128 so the check itself cannot wrap).
        prop_assert_eq!(
            u128::from(split.passback) + u128::from(split.referral) + u128::from(split.treasury),
            u128::from(charge)
        );
        prop_assert_eq!(split.credits.len(), amounts.len());
        prop_assert_eq!(split.credits.iter().map(|c| u128::from(*c)).sum::<u128>(), u128::from(split.passback));
        prop_assert_eq!(split.referral, referral);
        prop_assert!(split.passback <= pool);
        // The pro-rata remainder is below one micro-USDC per recipient and goes to treasury; with
        // no recipient the whole pool does.
        let remainder = pool - split.passback;
        if amounts.is_empty() {
            prop_assert_eq!(remainder, pool);
        } else {
            prop_assert!(remainder < amounts.len() as u64);
        }
        prop_assert_eq!(split.treasury, charge - pool - referral + remainder);
        // Rounding is monotonic: a larger board amount never earns a smaller credit.
        for (i, a) in amounts.iter().enumerate() {
            for (j, b) in amounts.iter().enumerate() {
                if a >= b {
                    prop_assert!(split.credits[i] >= split.credits[j]);
                }
            }
        }
    }

    #[test]
    fn split_errors_instead_of_panicking_for_any_input(
        charge in any::<u64>(),
        passback_bps in any::<u16>(),
        referral_bps in any::<u16>(),
        has_referrer in any::<bool>(),
        amounts in prop::collection::vec(any::<u64>(), 0..=TOP_LEN),
    ) {
        let weight_fits = amounts.iter().try_fold(0u64, |sum, a| sum.checked_add(*a)).is_some();
        match split_payment(charge, passback_bps, referral_bps, has_referrer, &amounts) {
            Ok(split) => {
                prop_assert!(weight_fits);
                prop_assert_eq!(
                    u128::from(split.passback) + u128::from(split.referral) + u128::from(split.treasury),
                    u128::from(charge)
                );
            }
            Err(_) => {
                let bps_fit = u32::from(passback_bps) + if has_referrer { u32::from(referral_bps) } else { 0 } <= 10_000;
                let zero_weight = !amounts.is_empty() && amounts.iter().all(|a| *a == 0);
                // Only an unrepresentable weight, an over-allocated split or a zero weight fail.
                prop_assert!(!weight_fits || !bps_fit || zero_weight);
            }
        }
    }
}

#[test]
fn split_at_u64_bounds() {
    let max = split_payment(u64::MAX, PASSBACK_BPS, REFERRAL_BPS, true, &[u64::MAX]).unwrap();
    assert_eq!(
        u128::from(max.passback) + u128::from(max.referral) + u128::from(max.treasury),
        u128::from(u64::MAX)
    );
    let full = split_payment(u64::MAX, 10_000, 0, false, &[1, u64::MAX - 1]).unwrap();
    assert_eq!(full.passback + full.treasury, u64::MAX);
    assert!(split_payment(u64::MAX, PASSBACK_BPS, REFERRAL_BPS, true, &[u64::MAX, 1]).is_err());
    assert!(split_payment(u64::MAX, 10_000, 1, true, &[1]).is_err());
    let none = split_payment(u64::MAX, PASSBACK_BPS, REFERRAL_BPS, false, &[]).unwrap();
    assert_eq!(
        (none.passback, none.referral, none.treasury),
        (0, 0, u64::MAX)
    );
}

#[test]
fn category_ids_are_bounded() {
    // F8: a moderator edit or a creation with an id the indexer cannot know must fail.
    for id in [1, 10, 190, MAX_CATEGORY_ID] {
        assert!(check_category(id).is_ok(), "{id}");
    }
    for id in [0, MAX_CATEGORY_ID + 1, 999, u16::MAX] {
        assert!(check_category(id).is_err(), "{id}");
    }
}

#[test]
fn passback_weights_are_read_at_execution() {
    // R12, accepted (spec §3.3): a ranked project that raises its own amount before a pending
    // payment executes takes a larger pro-rata share. The pool is fixed by the signed charge, so
    // only the other recipients' shares move; the payer's outflows do not.
    let entry = |i: usize, dollars: u64, seq: u64| TopEntry {
        project: key(i),
        amount: dollars * DOLLAR,
        reached_seq: seq,
    };
    let payer = key(9);
    let charge = 106 * DOLLAR;
    let credits = |top: &[TopEntry]| {
        let passed = passed_entries(top, &payer, 0, 0, charge, 10);
        let amounts: Vec<u64> = passed.iter().map(|e| e.amount).collect();
        let split = split_payment(charge, PASSBACK_BPS, REFERRAL_BPS, false, &amounts).unwrap();
        let by_project: Vec<(Pubkey, u64)> = passed
            .iter()
            .map(|e| e.project)
            .zip(split.credits.iter().copied())
            .collect();
        (by_project, split)
    };
    let quoted = vec![entry(0, 100, 1), entry(1, 50, 2), entry(2, 25, 3)];
    let raised = vec![entry(0, 100, 1), entry(2, 99, 4), entry(1, 50, 2)];
    let (before, quoted_split) = credits(&quoted);
    let (after, executed_split) = credits(&raised);
    let credit =
        |list: &[(Pubkey, u64)], i: usize| list.iter().find(|(p, _)| *p == key(i)).unwrap().1;
    assert!(credit(&after, 2) > credit(&before, 2));
    assert!(credit(&after, 0) < credit(&before, 0));
    let pool = ratio(charge, u64::from(PASSBACK_BPS), 10_000).unwrap();
    for split in [&quoted_split, &executed_split] {
        assert!(split.passback <= pool && pool - split.passback < 3);
        assert_eq!(split.passback + split.referral + split.treasury, charge);
    }
}

#[test]
fn project_day_size_matches_its_borsh_layout() {
    // R13: `pay` allocates `8 + ProjectDay::INIT_SPACE` bytes. The stored bump grew the account
    // from 80 to 81 bytes with no migration, so an earlier deployment's ProjectDays no longer
    // decode; deployments start fresh from this layout (onchain/README.md). Changing it again
    // needs a migration instruction, not an in-place upgrade.
    use anchor_lang::{AccountDeserialize, AccountSerialize, Space};
    let day = ProjectDay {
        project: key(0),
        day: u64::MAX,
        amount: u64::MAX,
        passback_accrued: u64::MAX,
        passback_claimed: u64::MAX,
        reached_seq: u64::MAX,
        bump: u8::MAX,
    };
    let mut bytes = Vec::new();
    day.try_serialize(&mut bytes).unwrap();
    assert_eq!(bytes.len(), 8 + ProjectDay::INIT_SPACE);
    assert_eq!(8 + ProjectDay::INIT_SPACE, 81);
    let decoded = ProjectDay::try_deserialize(&mut &bytes[..]).unwrap();
    assert_eq!((decoded.project, decoded.bump), (key(0), u8::MAX));
    // A pre-bump (80-byte) account fails to decode.
    assert!(ProjectDay::try_deserialize(&mut &bytes[..80]).is_err());
}

#[test]
fn creation_referrer_must_be_on_curve() {
    // F7 residual: a PDA referrer could never claim, stranding its 5% in Vault.
    assert_eq!(creation_referrer(Pubkey::default()).unwrap(), None);
    // An ed25519 public key (a wallet) is a curve point.
    let wallet = Pubkey::from_str_const("6bZwoFKYtKJWruiQ5iZbB2uoUA9a9QJfuzeU8xr8M9tu");
    assert!(wallet.is_on_curve());
    assert_eq!(creation_referrer(wallet).unwrap(), Some(wallet));
    let (pda, _) = Pubkey::find_program_address(&[b"config"], &crate::ID);
    assert!(!pda.is_on_curve());
    assert!(creation_referrer(pda).is_err());
}

#[test]
fn admission_expiry_is_bounded_to_an_hour() {
    // F18: an attestation expiring more than an hour past the validator clock is refused.
    let now = 1_790_000_000;
    for expires_at in [now, now + 300, now + MAX_ADMISSION_TTL] {
        assert!(
            check_admission_window(expires_at, now).is_ok(),
            "{expires_at}"
        );
    }
    for expires_at in [now - 1, now + MAX_ADMISSION_TTL + 1, i64::MAX, i64::MIN] {
        assert!(
            check_admission_window(expires_at, now).is_err(),
            "{expires_at}"
        );
    }
    assert!(check_admission_window(i64::MAX, i64::MAX - 1).is_ok());
}

#[test]
fn block_with_a_ranked_replacement_is_rejected_then_idempotent() {
    // F16: block #1 and supply #2, which is already ranked: the board would be one entry short.
    let entry = |i: usize, dollars: u64| TopEntry {
        project: key(i),
        amount: dollars * DOLLAR,
        reached_seq: i as u64 + 1,
    };
    let full: Vec<TopEntry> = (0..TOP_LEN).map(|i| entry(i, 100 - i as u64)).collect();
    let mut top = full.clone();
    assert!(block_entry(&mut top, &key(0), Some(entry(1, 99))).is_err());
    // The correct replacement fills the freed slot.
    let mut top = full.clone();
    block_entry(&mut top, &key(0), Some(entry(TOP_LEN, 5))).unwrap();
    assert_eq!(top.len(), TOP_LEN);
    assert!(top.iter().any(|e| e.project == key(TOP_LEN)));
    // Re-sending the same block is a no-op, as is a re-sent block naming another ranked project.
    let settled = snapshot(&top);
    block_entry(&mut top, &key(0), Some(entry(TOP_LEN, 5))).unwrap();
    block_entry(&mut top, &key(0), Some(entry(1, 99))).unwrap();
    assert_eq!(snapshot(&top), settled);
    // The blocked project can never be its own replacement.
    assert!(block_entry(&mut top, &key(0), Some(entry(0, 100))).is_err());
}

// ---------------------------------------------------------------------------------------------
// state.rs: model-based test of the Today top 10
// ---------------------------------------------------------------------------------------------

/// More projects than board slots, so eviction, refill and replacement all happen.
const PROJECTS: usize = 14;

#[derive(Clone, Debug)]
enum Replacement {
    /// The moderator supplies the next active project by Today amount (spec §4.3).
    Next,
    /// No replacement supplied, even when one exists (the crank repairs the board).
    Skip,
    /// An arbitrary, possibly wrong or invalid candidate.
    Arbitrary(usize),
}
#[derive(Clone, Debug)]
enum Op {
    Pay(usize, u64),
    Refill(usize),
    Block(usize, Replacement),
    Unblock(usize),
    /// Run `refill_top` until nothing qualifies, trying candidates in a rotated order.
    Crank(usize),
}

fn op() -> impl Strategy<Value = Op> {
    let dollars = prop_oneof![
        1u64..=40,
        (1u64..=8).prop_map(|d| d * 5),
        Just(4u64),
        Just(6u64)
    ];
    let replacement = prop_oneof![
        2 => Just(Replacement::Next),
        1 => Just(Replacement::Skip),
        1 => (0..PROJECTS).prop_map(Replacement::Arbitrary),
    ];
    prop_oneof![
        8 => (0..PROJECTS, dollars).prop_map(|(i, d)| Op::Pay(i, d)),
        2 => (0..PROJECTS).prop_map(Op::Refill),
        2 => (0..PROJECTS, replacement).prop_map(|(i, r)| Op::Block(i, r)),
        2 => (0..PROJECTS).prop_map(Op::Unblock),
        1 => (0..PROJECTS).prop_map(Op::Crank),
    ]
}

/// Reference state: every project's Today amount and status, plus the board the program
/// functions maintain. The reference ranking is recomputed from scratch on every check.
#[derive(Default)]
struct Model {
    amount: [u64; PROJECTS],
    seq: [u64; PROJECTS],
    all_time: [u64; PROJECTS],
    blocked: [bool; PROJECTS],
    accrued: [u64; PROJECTS],
    seq_counter: u64,
    charges: u128,
    outflows: u128,
    top: Vec<TopEntry>,
}

impl Model {
    fn entry(&self, i: usize) -> TopEntry {
        TopEntry {
            project: key(i),
            amount: self.amount[i],
            reached_seq: self.seq[i],
        }
    }
    fn ranks_ahead(&self, a: usize, b: usize) -> bool {
        ahead(self.amount[a], self.seq[a], self.amount[b], self.seq[b])
    }
    /// Total order for the reference ranking: unpaid projects all share `(0, 0)`, so they fall
    /// back to their index. Paid projects never tie because every payment takes a fresh seq.
    fn ranks_ahead_total(&self, a: usize, b: usize) -> bool {
        self.ranks_ahead(a, b)
            || ((self.amount[a], self.seq[a]) == (self.amount[b], self.seq[b]) && a < b)
    }
    /// All projects in the total `(amount desc, reached_seq asc)` order, sorted independently of
    /// `insert_top` (selection by repeated `ranks_ahead`).
    fn ranking(&self, candidates: impl Iterator<Item = usize>) -> Vec<usize> {
        let mut remaining: Vec<usize> = candidates.collect();
        let mut ordered = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let best = (0..remaining.len())
                .find(|&i| {
                    remaining.iter().all(|&other| {
                        other == remaining[i] || self.ranks_ahead_total(remaining[i], other)
                    })
                })
                .expect("ranking keys are distinct");
            ordered.push(remaining.remove(best));
        }
        ordered
    }
    /// The board §4.4 requires once `refill_top` has converged.
    fn reference_top(&self) -> Vec<usize> {
        let mut active =
            self.ranking((0..PROJECTS).filter(|&i| !self.blocked[i] && self.amount[i] > 0));
        active.truncate(TOP_LEN);
        active
    }
    fn board(&self) -> Vec<usize> {
        self.top.iter().map(|e| index_of(&e.project)).collect()
    }
    fn converged(&self) -> bool {
        self.board() == self.reference_top()
    }
    fn ranked(&self, i: usize) -> bool {
        self.top.iter().any(|e| e.project == key(i))
    }

    /// Mirrors the board part of `pay` in lib.rs. Returns whether the payment executed.
    fn pay(&mut self, i: usize, dollars: u64) -> bool {
        if self.blocked[i] {
            return false;
        }
        let charge = dollars * DOLLAR;
        let minimum = if self.all_time[i] == 0 {
            MIN_FIRST
        } else {
            DOLLAR
        };
        if charge < minimum {
            return false;
        }
        let new_amount = self.amount[i] + charge;
        // Independent statement of the +$5 rule (spec §3.1).
        let leader_blocks = self.top.first().is_some_and(|leader| {
            leader.project != key(i)
                && new_amount > leader.amount
                && new_amount < leader.amount + TOP_JUMP
        });
        let jump = check_top_jump(&self.top, &key(i), new_amount, TOP_JUMP);
        assert_eq!(
            jump.is_err(),
            leader_blocks,
            "top-jump rule disagrees with the spec"
        );
        if jump.is_err() {
            return false;
        }
        let seq = self.seq_counter + 1;
        let passed = passed_entries(
            &self.top,
            &key(i),
            self.amount[i],
            self.seq[i],
            new_amount,
            seq,
        );

        // Spec §3.3 "passed": on the board before, ranked above the payer before and below after.
        let everyone = 0..PROJECTS;
        let before = self.ranking(everyone.clone());
        let pos = |order: &[usize], p: usize| order.iter().position(|&x| x == p).unwrap();
        let (old_amount, old_seq) = (self.amount[i], self.seq[i]);
        self.amount[i] = new_amount;
        self.seq[i] = seq;
        let after = self.ranking(everyone);
        self.amount[i] = old_amount;
        self.seq[i] = old_seq;
        let expected: Vec<usize> = self
            .board()
            .into_iter()
            .filter(|&q| {
                q != i && pos(&before, q) < pos(&before, i) && pos(&after, q) > pos(&after, i)
            })
            .collect();
        let actual: Vec<usize> = passed.iter().map(|e| index_of(&e.project)).collect();
        assert_eq!(
            actual, expected,
            "passed set differs from the spec definition"
        );

        let amounts: Vec<u64> = passed.iter().map(|e| e.amount).collect();
        let split =
            split_payment(charge, PASSBACK_BPS, REFERRAL_BPS, i % 3 == 0, &amounts).unwrap();
        for (entry, credit) in passed.iter().zip(&split.credits) {
            self.accrued[index_of(&entry.project)] += credit;
        }
        self.charges += u128::from(charge);
        self.outflows +=
            u128::from(split.passback) + u128::from(split.referral) + u128::from(split.treasury);

        self.seq_counter = seq;
        self.amount[i] = new_amount;
        self.seq[i] = seq;
        self.all_time[i] += charge;
        {
            let entry = self.entry(i);
            insert_top(&mut self.top, entry)
        };
        true
    }
    /// Mirrors `refill_top`.
    fn refill(&mut self, i: usize) -> bool {
        if self.blocked[i] || self.amount[i] == 0 {
            return false;
        }
        let qualifies = !self.ranked(i)
            && (self.top.len() < TOP_LEN
                || self
                    .top
                    .last()
                    .is_some_and(|last| self.ranks_ahead_entry(i, last)));
        let result = {
            let entry = self.entry(i);
            refill_entry(&mut self.top, entry)
        };
        assert_eq!(
            result.is_ok(),
            qualifies,
            "refill rule disagrees with the spec"
        );
        result.is_ok()
    }
    fn ranks_ahead_entry(&self, i: usize, entry: &TopEntry) -> bool {
        ahead(self.amount[i], self.seq[i], entry.amount, entry.reached_seq)
    }
    /// Mirrors `set_project_status(blocked = true)`; an invalid candidate fails the whole
    /// instruction, leaving the status and board unchanged.
    fn block(&mut self, i: usize, replacement: &Replacement) -> bool {
        let candidate = match replacement {
            Replacement::Skip => None,
            Replacement::Arbitrary(j) => Some(*j),
            Replacement::Next => self
                .ranking((0..PROJECTS).filter(|&j| {
                    j != i && !self.blocked[j] && self.amount[j] > 0 && !self.ranked(j)
                }))
                .first()
                .copied(),
        };
        if let Some(j) = candidate {
            if self.blocked[j] || j == i || self.amount[j] == 0 {
                return false;
            }
        }
        let was_ranked = self.ranked(i);
        let len = self.top.len();
        let replacement = candidate.map(|j| self.entry(j));
        let mut top = self.top.clone();
        let result = block_entry(&mut top, &key(i), replacement.clone());
        // F16: a block that frees a slot must fill it with an unranked project.
        assert_eq!(
            result.is_err(),
            was_ranked && candidate.is_some_and(|j| self.ranked(j)),
            "block repair rule disagrees with the spec"
        );
        if result.is_err() {
            return false;
        }
        self.blocked[i] = true;
        self.top = top;
        if candidate.is_some() {
            assert!(
                !was_ranked || self.top.len() == len,
                "block left a short board"
            );
        }
        // Re-sending the same block changes nothing.
        let mut again = self.top.clone();
        assert!(block_entry(&mut again, &key(i), replacement).is_ok());
        assert_eq!(
            snapshot(&again),
            snapshot(&self.top),
            "repeated block changed the board"
        );
        true
    }
    /// Mirrors `set_project_status(blocked = false)`.
    fn unblock(&mut self, i: usize) {
        self.blocked[i] = false;
        if self.amount[i] > 0 {
            {
                let entry = self.entry(i);
                insert_top(&mut self.top, entry)
            };
        }
    }
    fn crank(&mut self, rotation: usize) {
        for _ in 0..PROJECTS * PROJECTS {
            let next = (0..PROJECTS).map(|k| (k + rotation) % PROJECTS).find(|&j| {
                !self.blocked[j]
                    && self.amount[j] > 0
                    && !self.ranked(j)
                    && (self.top.len() < TOP_LEN
                        || self
                            .top
                            .last()
                            .is_some_and(|last| self.ranks_ahead_entry(j, last)))
            });
            match next {
                Some(j) => assert!(self.refill(j)),
                None => return,
            }
        }
        panic!("refill_top did not converge");
    }

    /// §4.4 board invariants that hold after every instruction, converged or not.
    fn check(&self) {
        assert!(self.top.len() <= TOP_LEN);
        for pair in self.top.windows(2) {
            assert!(
                ahead(
                    pair[0].amount,
                    pair[0].reached_seq,
                    pair[1].amount,
                    pair[1].reached_seq
                ),
                "board out of (amount desc, reached_seq asc) order"
            );
        }
        let unique: BTreeSet<_> = self.top.iter().map(|e| e.project).collect();
        assert_eq!(unique.len(), self.top.len(), "duplicate board entry");
        for entry in &self.top {
            let i = index_of(&entry.project);
            assert!(!self.blocked[i], "blocked project on the board");
            assert!(entry.amount > 0);
            assert_eq!(
                (entry.amount, entry.reached_seq),
                (self.amount[i], self.seq[i]),
                "stale board snapshot"
            );
        }
        for i in 0..PROJECTS {
            assert!(self.amount[i] <= self.all_time[i]);
        }
        assert_eq!(self.charges, self.outflows);
    }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn top_ten_matches_reference_model(ops in prop::collection::vec(op(), 1..120)) {
        let mut model = Model::default();
        for op in &ops {
            let was_converged = model.converged();
            match op {
                Op::Pay(i, dollars) => {
                    model.pay(*i, *dollars);
                    // A payment never breaks a converged board.
                    prop_assert!(!was_converged || model.converged(), "pay broke convergence: {:?}", op);
                }
                Op::Refill(i) => {
                    let refilled = model.refill(*i);
                    prop_assert!(!(was_converged && refilled), "refill changed a converged board");
                }
                Op::Block(i, replacement) => {
                    model.block(*i, replacement);
                    if matches!(replacement, Replacement::Next) {
                        prop_assert!(!was_converged || model.converged(), "block with the next project broke convergence");
                    }
                }
                Op::Unblock(i) => {
                    model.unblock(*i);
                    prop_assert!(!was_converged || model.converged(), "unblock broke convergence");
                }
                Op::Crank(rotation) => {
                    model.crank(*rotation);
                    // §4.4: no active project outside the board ranks above its lowest entry.
                    prop_assert_eq!(model.board(), model.reference_top());
                }
            }
            model.check();
        }
        model.crank(0);
        prop_assert_eq!(model.board(), model.reference_top());
        model.check();
    }

    #[test]
    fn insert_top_is_sorted_truncated_and_idempotent(
        entries in prop::collection::vec((0..PROJECTS, 1u64..=50, any::<bool>()), 0..40),
    ) {
        let mut top = Vec::new();
        let mut latest = std::collections::BTreeMap::new();
        for (step, (i, dollars, early)) in entries.into_iter().enumerate() {
            // Unique seqs as on chain; `early` reuses an old-looking seq to mimic a refill.
            let seq = if early { step as u64 } else { 1_000 + step as u64 };
            let entry = TopEntry { project: key(i), amount: dollars * DOLLAR, reached_seq: seq };
            insert_top(&mut top, entry.clone());
            latest.insert(i, entry.clone());
            prop_assert!(top.len() <= TOP_LEN);
            let unique: BTreeSet<_> = top.iter().map(|e| e.project).collect();
            prop_assert_eq!(unique.len(), top.len());
            for pair in top.windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                prop_assert!(ahead(a.amount, a.reached_seq, b.amount, b.reached_seq));
            }
            // Re-inserting the same snapshot changes nothing.
            let snapshot: Vec<_> = top.iter().map(|e| (e.project, e.amount, e.reached_seq)).collect();
            if top.iter().any(|e| e.project == entry.project) {
                insert_top(&mut top, entry);
                let again: Vec<_> = top.iter().map(|e| (e.project, e.amount, e.reached_seq)).collect();
                prop_assert_eq!(again, snapshot);
            }
            // Every entry on the board is that project's latest snapshot.
            for e in &top {
                let last = &latest[&index_of(&e.project)];
                prop_assert_eq!((e.amount, e.reached_seq), (last.amount, last.reached_seq));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Cross-language vectors: onchain/tests/client-properties.test.ts replays these through
// `quotePayment` and requires identical results.
// ---------------------------------------------------------------------------------------------

const VECTOR_SEED: u64 = 0x5eed_0f_b0a2d;
const VECTOR_COUNT: usize = 96;
const VECTOR_PATH: &str = "../../onchain/tests/fixtures/passback-split-vectors.json";

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn split_vectors() -> String {
    let mut rng = XorShift(VECTOR_SEED);
    let mut lines = Vec::new();
    for _ in 0..VECTOR_COUNT {
        let size = rng.below(TOP_LEN as u64 + 1) as usize;
        let mut top: Vec<TopEntry> = Vec::new();
        let mut seq = 0u64;
        while top.len() < size {
            seq += 1 + rng.below(3);
            let project = rng.below(PROJECTS as u64) as usize;
            // Coarse amounts create ties broken by reached_seq.
            let dollars = if rng.below(3) == 0 {
                5 * (1 + rng.below(6))
            } else {
                1 + rng.below(400)
            };
            insert_top(
                &mut top,
                TopEntry {
                    project: key(project),
                    amount: dollars * DOLLAR,
                    reached_seq: seq,
                },
            );
        }
        let payer = rng.below(PROJECTS as u64) as usize;
        let (old_amount, old_seq) = match top.iter().find(|e| e.project == key(payer)) {
            Some(entry) => (entry.amount, entry.reached_seq),
            None if rng.below(2) == 0 => (0, 0),
            None => {
                let ceiling = top.last().map_or(50, |last| last.amount / DOLLAR);
                seq += 1;
                ((1 + rng.below(ceiling.max(1))) * DOLLAR, seq)
            }
        };
        let charge = match rng.below(4) {
            0 => (1 + rng.below(999_999)) * DOLLAR,
            _ => (1 + rng.below(600)) * DOLLAR,
        };
        let new_amount = old_amount + charge;
        let new_seq = seq + 1;
        let has_referrer = rng.below(2) == 0;
        let passed = passed_entries(&top, &key(payer), old_amount, old_seq, new_amount, new_seq);
        let amounts: Vec<u64> = passed.iter().map(|e| e.amount).collect();
        let split =
            split_payment(charge, PASSBACK_BPS, REFERRAL_BPS, has_referrer, &amounts).unwrap();
        let board = top
            .iter()
            .map(|e| {
                format!(
                    "[{},\"{}\",\"{}\"]",
                    index_of(&e.project),
                    e.amount,
                    e.reached_seq
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let passed = passed
            .iter()
            .map(|e| index_of(&e.project).to_string())
            .collect::<Vec<_>>()
            .join(",");
        let credits = split
            .credits
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(",");
        lines.push(format!(
            "{{\"top\":[{board}],\"project\":{payer},\"oldAmount\":\"{old_amount}\",\"oldSeq\":\"{old_seq}\",\"newAmount\":\"{new_amount}\",\"hasReferrer\":{has_referrer},\"passed\":[{passed}],\"credits\":[{credits}],\"passback\":\"{}\",\"referral\":\"{}\",\"treasury\":\"{}\"}}",
            split.passback, split.referral, split.treasury
        ));
    }
    format!(
        "{{\"seed\":\"{VECTOR_SEED}\",\"passbackBps\":{PASSBACK_BPS},\"referralBps\":{REFERRAL_BPS},\"vectors\":[\n{}\n]}}\n",
        lines.join(",\n")
    )
}

/// Regenerate with `PASSBACK_UPDATE_VECTORS=1 cargo test -p passback split_vectors`.
#[test]
fn split_vectors_fixture_is_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(VECTOR_PATH);
    let generated = split_vectors();
    if std::env::var_os("PASSBACK_UPDATE_VECTORS").is_some() {
        std::fs::write(&path, &generated).unwrap();
    }
    let stored = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        stored == generated,
        "{} is stale; regenerate it with PASSBACK_UPDATE_VECTORS=1 cargo test -p passback split_vectors",
        path.display()
    );
}

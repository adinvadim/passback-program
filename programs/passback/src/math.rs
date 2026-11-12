use crate::BoardError;
use anchor_lang::prelude::*;

pub const DOLLAR: u64 = 1_000_000;
pub fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or(error!(BoardError::Overflow))
}
pub fn sub(a: u64, b: u64) -> Result<u64> {
    a.checked_sub(b).ok_or(error!(BoardError::Overflow))
}
pub fn ratio(amount: u64, numerator: u64, denominator: u64) -> Result<u64> {
    require!(denominator > 0, BoardError::Overflow);
    let value = u128::from(amount)
        .checked_mul(u128::from(numerator))
        .ok_or(error!(BoardError::Overflow))?
        / u128::from(denominator);
    u64::try_from(value).map_err(|_| error!(BoardError::Overflow))
}
pub fn ahead(amount: u64, seq: u64, other_amount: u64, other_seq: u64) -> bool {
    amount > other_amount || (amount == other_amount && seq < other_seq)
}

/// Outflows of one payment (spec §3.3). `credits[i]` is the Passback credited to the i-th passed
/// entry; `treasury` takes the rest, including the pro-rata rounding remainder and every share
/// without a recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub credits: Vec<u64>,
    pub passback: u64,
    pub referral: u64,
    pub treasury: u64,
}

/// Splits `charge` between the passed entries (weighted by their board amounts), the referrer and
/// the treasury. Errors instead of panicking on any overflow or an impossible split.
pub fn split_payment(
    charge: u64,
    passback_bps: u16,
    referral_bps: u16,
    has_referrer: bool,
    passed_amounts: &[u64],
) -> Result<Split> {
    let weight = passed_amounts
        .iter()
        .try_fold(0u64, |sum, amount| add(sum, *amount))?;
    let pool = ratio(charge, u64::from(passback_bps), 10000)?;
    let mut credits = Vec::with_capacity(passed_amounts.len());
    let mut passback = 0u64;
    for amount in passed_amounts {
        let credit = ratio(pool, *amount, weight)?;
        passback = add(passback, credit)?;
        credits.push(credit);
    }
    let referral = if has_referrer {
        ratio(charge, u64::from(referral_bps), 10000)?
    } else {
        0
    };
    let treasury = sub(charge, add(passback, referral)?)?;
    Ok(Split {
        credits,
        passback,
        referral,
        treasury,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncapped_and_conservation() {
        assert_eq!(ratio(100 * DOLLAR, 3000, 10000).unwrap(), 30 * DOLLAR);
        let mut seed = 123456789u64;
        for _ in 0..10000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let payment = seed % (1000 * DOLLAR) + DOLLAR;
            let pool = ratio(payment, 3000, 10000).unwrap();
            let referral = ratio(payment, 500, 10000).unwrap();
            let weights = [seed % 1000 + 1, seed % 733 + 1, seed % 331 + 1];
            let total = weights.iter().sum();
            let allocated: u64 = weights
                .iter()
                .map(|w| ratio(pool, *w, total).unwrap())
                .sum();
            assert!(allocated <= pool);
            let treasury = sub(sub(payment, allocated).unwrap(), referral).unwrap();
            assert_eq!(allocated + referral + treasury, payment);
        }
    }
    #[test]
    fn overflow_and_ties() {
        assert!(add(u64::MAX, 1).is_err());
        assert!(sub(0, 1).is_err());
        assert_eq!(
            ratio(u64::MAX, 3000, 10000).unwrap(),
            (u128::from(u64::MAX) * 3 / 10) as u64
        );
        assert!(ahead(10, 1, 10, 2));
        assert!(!ahead(10, 2, 10, 1));
    }
}

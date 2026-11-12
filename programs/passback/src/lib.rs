use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::{
    load_current_index_checked, load_instruction_at_checked,
};
use anchor_spl::token::{self, Transfer};
use solana_sdk_ids::ed25519_program;
use solana_sha256_hasher::hash;
mod contexts;
#[cfg(test)]
mod fuzz;
mod math;
mod state;
pub use contexts::*;
use math::*;
pub use state::*;

declare_id!("C9NtyYCuGLZTPr6H9wLE2hhTsfZPmVdfiNucQxy2u3JB");

#[program]
pub mod passback {
    use super::*;
    pub fn initialize(ctx: Context<Initialize>, settings: ConfigSettings) -> Result<()> {
        require_keys_eq!(
            settings.authority,
            ctx.accounts.authority.key(),
            BoardError::Unauthorized
        );
        require_keys_eq!(
            settings.treasury,
            ctx.accounts.treasury.key(),
            BoardError::WrongAccount
        );
        let config = &mut ctx.accounts.config;
        config.usdc_mint = ctx.accounts.mint.key();
        config.passback_bps = 3000;
        config.referral_bps = 500;
        config.min_first_amount = 10 * DOLLAR;
        config.min_amount = DOLLAR;
        config.top_jump = 5 * DOLLAR;
        apply_settings(config, settings)?;
        emit!(ConfigUpdated {
            config_state: (**config).clone()
        });
        Ok(())
    }
    pub fn update_config(ctx: Context<UpdateConfig>, settings: ConfigSettings) -> Result<()> {
        require_keys_eq!(
            settings.treasury,
            ctx.accounts.treasury.key(),
            BoardError::WrongAccount
        );
        apply_settings(&mut ctx.accounts.config, settings)?;
        emit!(ConfigUpdated {
            config_state: (*ctx.accounts.config).clone()
        });
        Ok(())
    }
    pub fn pay<'info>(
        ctx: Context<'_, '_, '_, 'info, Pay<'info>>,
        identity_hash: [u8; 32],
        day: u64,
        new_day_amount: u64,
        referrer_wallet: Pubkey,
        creation: Option<Creation>,
        expected_charge: u64,
        expected_payout_wallet: Pubkey,
    ) -> Result<()> {
        check_day(day)?;
        let config = &mut ctx.accounts.config;
        require!(!config.paused, BoardError::Paused);
        let project = &mut ctx.accounts.project;
        if project.identity_key.is_empty() {
            let creation = creation.ok_or(error!(BoardError::AdmissionRequired))?;
            require!(
                !creation.identity_key.is_empty() && creation.identity_key.len() <= 256,
                BoardError::InvalidIdentity
            );
            require!(
                hash(creation.identity_key.as_bytes()).to_bytes() == identity_hash,
                BoardError::InvalidIdentity
            );
            check_category(creation.category_id)?;
            verify_admission(
                &ctx.accounts.instructions,
                config.admission_signer,
                identity_hash,
                creation.category_id,
                creation.expires_at,
            )?;
            project.identity_key = creation.identity_key;
            project.identity_hash = identity_hash;
            project.category_id = creation.category_id;
            project.creator = ctx.accounts.payer.key();
            project.payout_wallet = ctx.accounts.payer.key();
            project.created_at = Clock::get()?.unix_timestamp;
            project.referrer = creation_referrer(referrer_wallet)?;
        }
        require!(!project.blocked, BoardError::Blocked);
        require_keys_eq!(
            project.payout_wallet,
            expected_payout_wallet,
            BoardError::PayoutWalletChanged
        );
        require_keys_eq!(
            project.referrer.unwrap_or_default(),
            referrer_wallet,
            BoardError::WrongReferrer
        );
        let pd = &mut ctx.accounts.project_day;
        if pd.project == Pubkey::default() {
            pd.project = project.key();
            pd.day = day;
            pd.bump = ctx.bumps.project_day;
        }
        require!(
            pd.project == project.key() && pd.day == day,
            BoardError::WrongAccount
        );
        require!(
            new_day_amount > pd.amount && new_day_amount % DOLLAR == 0,
            BoardError::InvalidAmount
        );
        let charge = sub(new_day_amount, pd.amount)?;
        require!(charge == expected_charge, BoardError::StaleQuote);
        let minimum = if project.all_time_amount == 0 {
            config.min_first_amount
        } else {
            config.min_amount
        };
        require!(
            charge >= minimum && charge <= config.max_amount,
            BoardError::InvalidAmount
        );
        let board = &mut ctx.accounts.board;
        board.day = day;
        check_top_jump(&board.top, &project.key(), new_day_amount, config.top_jump)?;
        let seq = add(config.seq, 1)?;
        let passed = passed_entries(
            &board.top,
            &project.key(),
            pd.amount,
            pd.reached_seq,
            new_day_amount,
            seq,
        );
        let passed_amounts: Vec<u64> = passed.iter().map(|entry| entry.amount).collect();
        let split = split_payment(
            charge,
            config.passback_bps,
            config.referral_bps,
            project.referrer.is_some(),
            &passed_amounts,
        )?;
        for (entry, credit) in passed.iter().zip(split.credits.iter().copied()) {
            let info = locate_project_day(ctx.remaining_accounts, &entry.project, day)
                .ok_or(error!(BoardError::StaleBoard))?;
            let mut recipient = read_project_day(info, entry.project, day)?;
            require!(
                recipient.amount == entry.amount && recipient.reached_seq == entry.reached_seq,
                BoardError::StaleBoard
            );
            recipient.passback_accrued = add(recipient.passback_accrued, credit)?;
            emit!(PassbackCredited {
                amount: credit,
                project_day_state: recipient.clone()
            });
            write_project_day(info, &recipient)?;
        }
        let credited = split.passback;
        let referral_amount = split.referral;
        let referrer = &mut ctx.accounts.referrer;
        referrer.wallet = referrer_wallet;
        referrer.accrued = add(referrer.accrued, referral_amount)?;
        let vault_amount = add(credited, referral_amount)?;
        let treasury_amount = split.treasury;
        for (destination, amount) in [
            (ctx.accounts.vault.to_account_info(), vault_amount),
            (ctx.accounts.treasury.to_account_info(), treasury_amount),
        ] {
            if amount > 0 {
                token::transfer(
                    CpiContext::new(
                        ctx.accounts.token_program.to_account_info(),
                        Transfer {
                            from: ctx.accounts.payer_token.to_account_info(),
                            to: destination,
                            authority: ctx.accounts.payer.to_account_info(),
                        },
                    ),
                    amount,
                )?;
            }
        }
        config.seq = seq;
        pd.amount = new_day_amount;
        pd.reached_seq = seq;
        project.all_time_amount = add(project.all_time_amount, charge)?;
        project.reached_seq = seq;
        board.volume = add(board.volume, charge)?;
        board.payments = add(board.payments, 1)?;
        insert_top(
            &mut board.top,
            TopEntry {
                project: project.key(),
                amount: pd.amount,
                reached_seq: seq,
            },
        );
        emit!(Paid {
            project: project.key(),
            payer: ctx.accounts.payer.key(),
            amount: charge,
            treasury_amount,
            referral_amount,
            passback_amount: credited,
            project_state: (***project).clone(),
            project_day_state: (***pd).clone(),
            referrer_state: (***referrer).clone(),
            board_state: (***board).clone(),
            config_state: (***config).clone()
        });
        Ok(())
    }
    pub fn claim<'info>(
        ctx: Context<'_, '_, '_, 'info, Claim<'info>>,
        days: Vec<u64>,
    ) -> Result<()> {
        require!(
            !days.is_empty() && days.len() <= 20 && days.len() == ctx.remaining_accounts.len(),
            BoardError::WrongAccount
        );
        let mut total = 0u64;
        for (index, day) in days.iter().enumerate() {
            require!(!days[..index].contains(day), BoardError::DuplicateDay);
            let info = &ctx.remaining_accounts[index];
            let mut pd = read_project_day(info, ctx.accounts.project.key(), *day)?;
            let amount = sub(pd.passback_accrued, pd.passback_claimed)?;
            total = add(total, amount)?;
            pd.passback_claimed = pd.passback_accrued;
            emit!(Claimed {
                payout_wallet: ctx.accounts.wallet.key(),
                amount,
                project_day_state: pd.clone()
            });
            write_project_day(info, &pd)?;
        }
        transfer_claim(
            &ctx.accounts.token_program,
            &ctx.accounts.config,
            &ctx.accounts.vault,
            &ctx.accounts.destination,
            ctx.bumps.config,
            total,
        )?;
        Ok(())
    }
    pub fn claim_referral(ctx: Context<ClaimReferral>) -> Result<()> {
        let referrer = &mut ctx.accounts.referrer;
        let amount = sub(referrer.accrued, referrer.claimed)?;
        referrer.claimed = referrer.accrued;
        transfer_claim(
            &ctx.accounts.token_program,
            &ctx.accounts.config,
            &ctx.accounts.vault,
            &ctx.accounts.destination,
            ctx.bumps.config,
            amount,
        )?;
        emit!(ReferralClaimed {
            amount,
            referrer_state: (**referrer).clone()
        });
        Ok(())
    }
    pub fn refill_top(ctx: Context<RefillTop>, day: u64) -> Result<()> {
        check_day(day)?;
        require!(!ctx.accounts.project.blocked, BoardError::Blocked);
        let pd = &ctx.accounts.project_day;
        require!(pd.amount > 0, BoardError::InvalidAmount);
        let board = &mut ctx.accounts.board;
        board.day = day;
        refill_entry(
            &mut board.top,
            TopEntry {
                project: ctx.accounts.project.key(),
                amount: pd.amount,
                reached_seq: pd.reached_seq,
            },
        )?;
        emit!(TopRefilled {
            project: ctx.accounts.project.key(),
            board_state: (**board).clone()
        });
        Ok(())
    }
    pub fn close_project_day(ctx: Context<CloseProjectDay>, day: u64) -> Result<()> {
        require!(day < current_day()?, BoardError::DayNotEnded);
        require!(
            ctx.accounts.project_day.passback_accrued == ctx.accounts.project_day.passback_claimed,
            BoardError::UnclaimedFunds
        );
        emit!(ProjectDayClosed {
            project: ctx.accounts.project.key(),
            day,
            payout_wallet: ctx.accounts.wallet.key()
        });
        Ok(())
    }
    pub fn set_project_status<'info>(
        ctx: Context<'_, '_, '_, 'info, SetProjectStatus<'info>>,
        day: u64,
        blocked: bool,
    ) -> Result<()> {
        check_day(day)?;
        moderator(&ctx.accounts.config, ctx.accounts.actor.key())?;
        let project = &mut ctx.accounts.project;
        project.blocked = blocked;
        let board = &mut ctx.accounts.board;
        board.day = day;
        if blocked {
            require!(
                ctx.remaining_accounts.is_empty() || ctx.remaining_accounts.len() == 2,
                BoardError::WrongAccount
            );
            let mut replacement = None;
            if ctx.remaining_accounts.len() == 2 {
                let candidate_info = &ctx.remaining_accounts[0];
                require_keys_eq!(*candidate_info.owner, ID, BoardError::WrongAccount);
                let candidate =
                    Project::try_deserialize(&mut &candidate_info.try_borrow_data()?[..])?;
                require!(
                    !candidate.blocked && candidate_info.key() != project.key(),
                    BoardError::Blocked
                );
                let pd = read_project_day(&ctx.remaining_accounts[1], candidate_info.key(), day)?;
                require!(pd.amount > 0, BoardError::InvalidAmount);
                replacement = Some(TopEntry {
                    project: candidate_info.key(),
                    amount: pd.amount,
                    reached_seq: pd.reached_seq,
                });
            }
            block_entry(&mut board.top, &project.key(), replacement)?;
        } else {
            require!(ctx.remaining_accounts.len() == 1, BoardError::WrongAccount);
            let info = &ctx.remaining_accounts[0];
            require_keys_eq!(
                info.key(),
                Pubkey::find_program_address(
                    &[b"project_day", project.key().as_ref(), &day.to_le_bytes()],
                    &ID
                )
                .0,
                BoardError::WrongAccount
            );
            if !info.data_is_empty() {
                let pd = read_project_day(info, project.key(), day)?;
                if pd.amount > 0 {
                    insert_top(
                        &mut board.top,
                        TopEntry {
                            project: project.key(),
                            amount: pd.amount,
                            reached_seq: pd.reached_seq,
                        },
                    );
                }
            }
        }
        emit!(ProjectStatusChanged {
            project: project.key(),
            project_state: (**project).clone(),
            board_state: (**board).clone()
        });
        Ok(())
    }
    pub fn set_category(ctx: Context<EditProject>, category_id: u16) -> Result<()> {
        moderator(&ctx.accounts.config, ctx.accounts.actor.key())?;
        check_category(category_id)?;
        ctx.accounts.project.category_id = category_id;
        emit!(CategoryChanged {
            project: ctx.accounts.project.key(),
            project_state: (*ctx.accounts.project).clone()
        });
        Ok(())
    }
    pub fn set_payout_wallet(ctx: Context<EditProject>, payout_wallet: Pubkey) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.actor.key(),
            ctx.accounts.config.authority,
            BoardError::Unauthorized
        );
        require!(payout_wallet != Pubkey::default(), BoardError::WrongAccount);
        ctx.accounts.project.payout_wallet = payout_wallet;
        emit!(PayoutWalletChanged {
            project: ctx.accounts.project.key(),
            project_state: (*ctx.accounts.project).clone()
        });
        Ok(())
    }
    pub fn set_paused(ctx: Context<SetPaused>, paused: bool) -> Result<()> {
        moderator(&ctx.accounts.config, ctx.accounts.actor.key())?;
        ctx.accounts.config.paused = paused;
        emit!(PausedChanged {
            config_state: (*ctx.accounts.config).clone()
        });
        Ok(())
    }
}

fn apply_settings(config: &mut Config, settings: ConfigSettings) -> Result<()> {
    require!(
        settings.authority != Pubkey::default()
            && settings.moderator != Pubkey::default()
            && settings.admission_signer != Pubkey::default(),
        BoardError::WrongAccount
    );
    require!(
        settings.max_amount >= 10 * DOLLAR
            && settings.max_amount <= 999_999 * DOLLAR
            && settings.max_amount % DOLLAR == 0,
        BoardError::InvalidAmount
    );
    config.authority = settings.authority;
    config.moderator = settings.moderator;
    config.admission_signer = settings.admission_signer;
    config.treasury = settings.treasury;
    config.max_amount = settings.max_amount;
    Ok(())
}
fn current_day() -> Result<u64> {
    u64::try_from(Clock::get()?.unix_timestamp / 86400).map_err(|_| error!(BoardError::WrongDay))
}
fn check_day(day: u64) -> Result<()> {
    require!(day == current_day()?, BoardError::WrongDay);
    Ok(())
}
fn moderator(config: &Config, actor: Pubkey) -> Result<()> {
    require!(
        actor == config.authority || actor == config.moderator,
        BoardError::Unauthorized
    );
    Ok(())
}
/// Finds the `ProjectDay` of `(project, day)` among `accounts` by its stored fields, without
/// deriving an address. `read_project_day` then verifies the account and its address, so a
/// payment's cost per passed project is fixed whatever that project's PDA bump is (F1).
fn locate_project_day<'a, 'info>(
    accounts: &'a [AccountInfo<'info>],
    project: &Pubkey,
    day: u64,
) -> Option<&'a AccountInfo<'info>> {
    accounts.iter().find(|info| {
        *info.owner == ID
            && info.try_borrow_data().is_ok_and(|data| {
                data.len() >= 48
                    && data[..8] == *ProjectDay::DISCRIMINATOR
                    && data[8..40] == *project.as_ref()
                    && data[40..48] == day.to_le_bytes()
            })
    })
}
fn read_project_day(info: &AccountInfo, project: Pubkey, day: u64) -> Result<ProjectDay> {
    require_keys_eq!(*info.owner, ID, BoardError::WrongAccount);
    let pd = ProjectDay::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    require!(
        pd.project == project && pd.day == day,
        BoardError::WrongAccount
    );
    // The stored bump is the canonical one `pay` found at creation; one checked derivation.
    let address = Pubkey::create_program_address(
        &[
            b"project_day",
            project.as_ref(),
            &day.to_le_bytes(),
            &[pd.bump],
        ],
        &ID,
    )
    .map_err(|_| error!(BoardError::WrongAccount))?;
    require_keys_eq!(info.key(), address, BoardError::WrongAccount);
    Ok(pd)
}
fn write_project_day(info: &AccountInfo, pd: &ProjectDay) -> Result<()> {
    require!(info.is_writable, BoardError::StaleBoard);
    pd.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
    Ok(())
}
fn transfer_claim<'info>(
    program: &Program<'info, token::Token>,
    config: &Account<'info, Config>,
    vault: &Account<'info, token::TokenAccount>,
    destination: &Account<'info, token::TokenAccount>,
    bump: u8,
    amount: u64,
) -> Result<()> {
    if amount > 0 {
        let bump_seed = [bump];
        let signer: &[&[u8]] = &[b"config", &bump_seed];
        token::transfer(
            CpiContext::new_with_signer(
                program.to_account_info(),
                Transfer {
                    from: vault.to_account_info(),
                    to: destination.to_account_info(),
                    authority: config.to_account_info(),
                },
                &[signer],
            ),
            amount,
        )?;
    }
    Ok(())
}
fn verify_admission(
    instructions: &AccountInfo,
    signer: Pubkey,
    identity_hash: [u8; 32],
    category: u16,
    expires_at: i64,
) -> Result<()> {
    check_admission_window(expires_at, Clock::get()?.unix_timestamp)?;
    let index = load_current_index_checked(instructions)?;
    require!(index > 0, BoardError::AdmissionRequired);
    let ix = load_instruction_at_checked(usize::from(index - 1), instructions)?;
    require_keys_eq!(
        ix.program_id,
        ed25519_program::ID,
        BoardError::AdmissionRequired
    );
    // Accept only a single self-contained precompile payload, never cross-instruction offsets.
    let data = ix.data;
    require!(
        ix.accounts.is_empty() && data.len() == 212 && data[0] == 1 && data[1] == 0,
        BoardError::AdmissionRequired
    );
    let word = |offset: usize| u16::from_le_bytes([data[offset], data[offset + 1]]);
    require!(
        word(2) == 48
            && word(4) == u16::MAX
            && word(6) == 16
            && word(8) == u16::MAX
            && word(10) == 112
            && word(12) == 100
            && word(14) == u16::MAX,
        BoardError::AdmissionRequired
    );
    require!(
        &data[16..48] == signer.as_ref(),
        BoardError::AdmissionRequired
    );
    let mut message = Vec::with_capacity(100);
    message.extend_from_slice(b"100isthenew50:admission:v1");
    message.extend_from_slice(ID.as_ref());
    message.extend_from_slice(&identity_hash);
    message.extend_from_slice(&category.to_le_bytes());
    message.extend_from_slice(&expires_at.to_le_bytes());
    require!(data[112..] == message, BoardError::AdmissionRequired);
    Ok(())
}
#[error_code]
pub enum BoardError {
    #[msg("Arithmetic overflow")]
    Overflow,
    #[msg("Payments are paused")]
    Paused,
    #[msg("Project is blocked")]
    Blocked,
    #[msg("Whole-dollar payment is outside the permitted range")]
    InvalidAmount,
    #[msg("Taking first place requires a five-dollar lead")]
    TopJumpTooSmall,
    #[msg("The board changed; fetch a fresh quote and sign again")]
    StaleBoard,
    #[msg("The validator day changed; fetch a fresh quote")]
    WrongDay,
    #[msg("Invalid project identity")]
    InvalidIdentity,
    #[msg("Valid admission signature required")]
    AdmissionRequired,
    #[msg("Admission expired")]
    AdmissionExpired,
    #[msg("Incorrect referrer")]
    WrongReferrer,
    #[msg("Incorrect account")]
    WrongAccount,
    #[msg("Incorrect USDC mint")]
    WrongMint,
    #[msg("Unauthorized signer")]
    Unauthorized,
    #[msg("Project already appears on this board")]
    AlreadyRanked,
    #[msg("Project does not qualify for the top ten")]
    DoesNotQualify,
    #[msg("Duplicate claim day")]
    DuplicateDay,
    #[msg("The day has not ended")]
    DayNotEnded,
    #[msg("Claim the remaining balance before closing")]
    UnclaimedFunds,
    #[msg("The payment amount changed; review a fresh quote and sign again")]
    StaleQuote,
    #[msg("The payout wallet changed; review the recipient and sign again")]
    PayoutWalletChanged,
    #[msg("Category id is outside 1-255")]
    InvalidCategory,
}

use crate::{state::*, BoardError};
use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ BoardError::WrongAccount)]
    pub program: Program<'info, crate::program::Passback>,
    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ BoardError::Unauthorized)]
    pub program_data: Account<'info, ProgramData>,
    #[account(init, payer = authority, space = 8 + Config::INIT_SPACE, seeds = [b"config"], bump)]
    pub config: Account<'info, Config>,
    #[account(constraint = mint.decimals == 6 @ BoardError::WrongMint)]
    pub mint: Account<'info, Mint>,
    #[account(token::mint = mint)]
    pub treasury: Account<'info, TokenAccount>,
    #[account(init, payer = authority, seeds = [b"vault"], bump, token::mint = mint, token::authority = config)]
    pub vault: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    pub authority: Signer<'info>,
    #[account(mut, seeds = [b"config"], bump, has_one = authority)]
    pub config: Account<'info, Config>,
    #[account(constraint = treasury.mint == config.usdc_mint @ BoardError::WrongMint,
      constraint = treasury.key() != Pubkey::find_program_address(&[b"vault"], &crate::ID).0 @ BoardError::WrongAccount)]
    pub treasury: Account<'info, TokenAccount>,
}
#[derive(Accounts)]
#[instruction(identity_hash: [u8;32], day: u64, new_day_amount: u64, referrer_wallet: Pubkey)]
pub struct Pay<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [b"config"], bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(init_if_needed, payer = payer, space = 8 + Project::INIT_SPACE, seeds = [b"project", identity_hash.as_ref()], bump)]
    pub project: Box<Account<'info, Project>>,
    #[account(init_if_needed, payer = payer, space = 8 + DayBoard::INIT_SPACE, seeds = [b"day".as_ref(), day.to_le_bytes().as_ref()], bump)]
    pub board: Box<Account<'info, DayBoard>>,
    #[account(init_if_needed, payer = payer, space = 8 + ProjectDay::INIT_SPACE, seeds = [b"project_day", project.key().as_ref(), &day.to_le_bytes()], bump)]
    pub project_day: Box<Account<'info, ProjectDay>>,
    #[account(init_if_needed, payer = payer, space = 8 + Referrer::INIT_SPACE, seeds = [b"referrer", referrer_wallet.as_ref()], bump)]
    pub referrer: Box<Account<'info, Referrer>>,
    #[account(mut, token::authority = payer, constraint = payer_token.mint == config.usdc_mint @ BoardError::WrongMint)]
    pub payer_token: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"vault"], bump, token::authority = config, constraint = vault.mint == config.usdc_mint @ BoardError::WrongMint)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, address = config.treasury, constraint = treasury.mint == config.usdc_mint @ BoardError::WrongMint)]
    pub treasury: Box<Account<'info, TokenAccount>>,
    /// CHECK: Fixed instructions sysvar; parsed by the admission verifier.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct Claim<'info> {
    pub wallet: Signer<'info>,
    #[account(seeds = [b"config"], bump)]
    pub config: Account<'info, Config>,
    #[account(constraint = project.payout_wallet == wallet.key() @ BoardError::Unauthorized)]
    pub project: Account<'info, Project>,
    #[account(mut, seeds = [b"vault"], bump, token::authority = config, constraint = vault.mint == config.usdc_mint @ BoardError::WrongMint)]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, token::authority = wallet, constraint = destination.mint == config.usdc_mint @ BoardError::WrongMint)]
    pub destination: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}
#[derive(Accounts)]
pub struct ClaimReferral<'info> {
    pub wallet: Signer<'info>,
    #[account(seeds = [b"config"], bump)]
    pub config: Account<'info, Config>,
    #[account(mut, seeds = [b"referrer", wallet.key().as_ref()], bump, constraint = referrer.wallet == wallet.key() @ BoardError::WrongAccount)]
    pub referrer: Account<'info, Referrer>,
    #[account(mut, seeds = [b"vault"], bump, token::authority = config, constraint = vault.mint == config.usdc_mint @ BoardError::WrongMint)]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, token::authority = wallet, constraint = destination.mint == config.usdc_mint @ BoardError::WrongMint)]
    pub destination: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}
#[derive(Accounts)]
#[instruction(day: u64)]
pub struct RefillTop<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub project: Account<'info, Project>,
    #[account(seeds = [b"project_day", project.key().as_ref(), &day.to_le_bytes()], bump = project_day.bump,
      constraint = project_day.project == project.key() && project_day.day == day @ BoardError::WrongAccount)]
    pub project_day: Account<'info, ProjectDay>,
    #[account(init_if_needed, payer = payer, space = 8 + DayBoard::INIT_SPACE, seeds = [b"day".as_ref(), day.to_le_bytes().as_ref()], bump)]
    pub board: Account<'info, DayBoard>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
#[instruction(day: u64)]
pub struct CloseProjectDay<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,
    #[account(constraint = project.payout_wallet == wallet.key() @ BoardError::Unauthorized)]
    pub project: Account<'info, Project>,
    #[account(mut, close = wallet, seeds = [b"project_day", project.key().as_ref(), &day.to_le_bytes()], bump = project_day.bump,
      constraint = project_day.project == project.key() && project_day.day == day @ BoardError::WrongAccount)]
    pub project_day: Account<'info, ProjectDay>,
}
#[derive(Accounts)]
#[instruction(day: u64)]
pub struct SetProjectStatus<'info> {
    #[account(mut)]
    pub actor: Signer<'info>,
    #[account(seeds = [b"config"], bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub project: Account<'info, Project>,
    #[account(init_if_needed, payer = actor, space = 8 + DayBoard::INIT_SPACE, seeds = [b"day".as_ref(), day.to_le_bytes().as_ref()], bump)]
    pub board: Account<'info, DayBoard>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct EditProject<'info> {
    pub actor: Signer<'info>,
    #[account(seeds = [b"config"], bump)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub project: Account<'info, Project>,
}
#[derive(Accounts)]
pub struct SetPaused<'info> {
    pub actor: Signer<'info>,
    #[account(mut, seeds = [b"config"], bump)]
    pub config: Account<'info, Config>,
}

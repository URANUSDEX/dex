use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
    program::{invoke, invoke_signed},
    system_instruction,
    sysvar::{rent::Rent, Sysvar},
};

solana_program::declare_id!("URAa3qGD1qVKKqyQrF8iBVZRTwa4Q8RkMd6Gx7u2KL1");

pub const DEX_PUBKEY: Pubkey = solana_program::pubkey!("URAbknhQPhFiY92S5iM9nhzoZC5Vkch7S5VERa4PmuV");
pub const DEX_FEES_PUBKEY: Pubkey = solana_program::pubkey!("URAfeAaGMoavvTe8vqPwMX6cUvTjq8WMG5c9nFo7Q8j");

pub const INSTRUCTION_INITIALIZE: u8 = 0;
pub const INSTRUCTION_DEX_MODIFY: u8 = 1;
pub const INSTRUCTION_USER_MODIFY: u8 = 2;
pub const INSTRUCTION_PROCESS_PNL: u8 = 3;
pub const INSTRUCTION_FORCE_CLOSE: u8 = 4;
pub const INSTRUCTION_MARKET_TRANSFER: u8 = 5;

pub const MIN_POSITION_SIZE_LAMPORTS: u64 = 10_000_000;
pub const BASE_FEE_BASIS_POINTS: u64 = 100;
pub const LEVERAGE_FEE_BASIS_POINTS: u64 = 10;
pub const MAXIMUM_LEVERAGE: u8 = 5;
pub const POSITION_LONG: i8 = 1;
pub const POSITION_SHORT: i8 = -1;

pub const MAX_SYMBOL_LENGTH: usize = 32;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct PositionAccount {
    pub owner: Pubkey,
    pub market_mint: Pubkey,
    pub market_symbol: [u8; MAX_SYMBOL_LENGTH],
    pub entry_price: u64,
    pub liquidation_price: u64,
    pub paid_amount: u64,
    pub position_size: u64,
    pub leverage: u8,
    pub closed: u8,
    pub position_nonce: u64,
    pub pnl: i64,
    pub direction: i8,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct InitializePositionData {
    pub market_mint: Pubkey,
    pub market_symbol: [u8; MAX_SYMBOL_LENGTH],
    pub paid_amount: u64,
    pub position_size: u64,
    pub leverage: u8,
    pub position_nonce: u64,
    pub direction: i8,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct DexModifyData {
    pub new_entry_price: u64,
    pub new_liquidation_price: u64,
    pub position_nonce: u64,
    pub new_close_state: u8,
    pub new_pnl: i64,
    pub new_market_mint: Pubkey,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct UserModifyData {
    pub close_position: bool,
    pub position_nonce: u64,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct ProcessPnlData {
    pub position_nonce: u64,
    pub final_pnl: i64,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct MarketTransferData {
    pub amount: u64,
    pub from_market_mint: Pubkey,
    pub to_market_mint: Pubkey,
    pub from_market_pda: Pubkey,
    pub to_market_pda: Pubkey,
}

pub fn fixed_array_to_string(array: &[u8; MAX_SYMBOL_LENGTH]) -> Result<String, ProgramError> {
    let end = array.iter().position(|&x| x == 0).unwrap_or(MAX_SYMBOL_LENGTH);
    
    match std::str::from_utf8(&array[..end]) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => {
            msg!("Invalid UTF-8 in market symbol");
            Err(ProgramError::InvalidAccountData)
        }
    }
}



entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.is_empty() {
        msg!("Empty instruction data");
        return Err(ProgramError::InvalidInstructionData);
    }

    let instruction_type = instruction_data[0];

    match instruction_type {
        INSTRUCTION_INITIALIZE => {
            if instruction_data.len() < 2 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let initialize_data = InitializePositionData::try_from_slice(&instruction_data[1..])?;
            process_initialize(program_id, accounts, initialize_data)
        },
        INSTRUCTION_DEX_MODIFY => {
            if instruction_data.len() < 2 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let dex_data = DexModifyData::try_from_slice(&instruction_data[1..])?;
            process_dex_modify(program_id, accounts, dex_data)
        },
        INSTRUCTION_USER_MODIFY => {
            if instruction_data.len() < 2 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let user_data = UserModifyData::try_from_slice(&instruction_data[1..])?;
            process_user_modify(program_id, accounts, user_data)
        },
        INSTRUCTION_PROCESS_PNL => {
            if instruction_data.len() < 2 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let pnl_data = ProcessPnlData::try_from_slice(&instruction_data[1..])?;
            process_pnl(program_id, accounts, pnl_data)
        },
        INSTRUCTION_FORCE_CLOSE => {
            process_force_close(program_id, accounts)
        },
        INSTRUCTION_MARKET_TRANSFER => {
            if instruction_data.len() < 2 {
                return Err(ProgramError::InvalidInstructionData);
            }
            let transfer_data = MarketTransferData::try_from_slice(&instruction_data[1..])?;
            process_market_transfer(program_id, accounts, transfer_data)
        },
        _ => {
            msg!("Invalid instruction type: {}", instruction_type);
            Err(ProgramError::InvalidInstructionData)
        }
    }
}

#[inline(always)]
fn find_position_address(
    owner: &Pubkey,
    position_nonce: u64,
    program_id: &Pubkey
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            b"uranus_position",
            owner.as_ref(),
            &position_nonce.to_le_bytes(),
        ],
        program_id,
    )
}

#[inline(always)]
fn find_market_address(
    market_mint: &Pubkey,
    program_id: &Pubkey
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            b"uranus_market",
            market_mint.as_ref(),
            b"v1",
        ],
        program_id,
    )
}

#[inline(always)]
fn find_program_vault_address(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            b"uranus_program_vault",
        ],
        program_id,
    )
}

fn process_initialize(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    initialize_data: InitializePositionData,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();
    
    let payer_account = next_account_info(accounts_iter)?;
    let owner_account = next_account_info(accounts_iter)?;
    let position_account = next_account_info(accounts_iter)?;
    let market_account = next_account_info(accounts_iter)?;
    let dex_account = next_account_info(accounts_iter)?;
    let dex_fees_account = next_account_info(accounts_iter)?;
    let system_program = next_account_info(accounts_iter)?;
    
    if !payer_account.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if initialize_data.position_size < MIN_POSITION_SIZE_LAMPORTS {
        msg!("Position size too small");
        return Err(ProgramError::InvalidArgument);
    }
    
    let leverage = initialize_data.leverage.clamp(1, MAXIMUM_LEVERAGE);
    
    if leverage != initialize_data.leverage {
        msg!("Leverage adjusted to {}x", leverage);
    }

    let base_fee = initialize_data.paid_amount
        .saturating_mul(BASE_FEE_BASIS_POINTS)
        .saturating_div(10000);
        
    let leverage_fee = initialize_data.paid_amount
        .saturating_mul(LEVERAGE_FEE_BASIS_POINTS)
        .saturating_mul(leverage as u64)
        .saturating_div(10000);
    
    let total_fee = base_fee.saturating_add(leverage_fee);
    let position_amount_after_fees = initialize_data.paid_amount.saturating_sub(total_fee);
    let actual_position_size = position_amount_after_fees.saturating_mul(leverage as u64);

    if actual_position_size < MIN_POSITION_SIZE_LAMPORTS {
        msg!("Position size after fees too small");
        return Err(ProgramError::InvalidArgument);
    }
    
    if initialize_data.direction != POSITION_LONG && initialize_data.direction != POSITION_SHORT {
        msg!("Invalid direction");
        return Err(ProgramError::InvalidArgument);
    }
    
    let (market_liquidity_pda, market_bump) = find_market_address(
        &initialize_data.market_mint,
        program_id
    );
    
    if market_account.key != &market_liquidity_pda {
        msg!("Invalid market account");
        return Err(ProgramError::InvalidArgument);
    }
    
    if dex_account.key != &DEX_PUBKEY {
        msg!("Invalid DEX account");
        return Err(ProgramError::InvalidArgument);
    }
    
    let (position_pda, bump_seed) = find_position_address(
        owner_account.key,
        initialize_data.position_nonce,
        program_id
    );
    
    if position_pda != *position_account.key {
        msg!("Invalid position account");
        return Err(ProgramError::InvalidArgument);
    }
    
    if market_account.data_is_empty() && market_account.lamports() == 0 {
        let rent = Rent::get()?;
        let minimum_balance = rent.minimum_balance(0);
        
        let market_liquidity_seeds = &[
            b"uranus_market",
            initialize_data.market_mint.as_ref(),
            b"v1",
            &[market_bump],
        ];
        
        invoke_signed(
            &system_instruction::create_account(
                payer_account.key,
                market_account.key,
                minimum_balance,
                0,
                program_id,
            ),
            &[
                payer_account.clone(),
                market_account.clone(),
                system_program.clone(),
            ],
            &[market_liquidity_seeds],
        )?;
    }
    
    let position = PositionAccount {
        owner: *owner_account.key,
        market_mint: initialize_data.market_mint,
        market_symbol: initialize_data.market_symbol,
        entry_price: 0,
        liquidation_price: 0,
        paid_amount: position_amount_after_fees,
        position_size: actual_position_size,
        leverage,
        closed: 0,
        position_nonce: initialize_data.position_nonce,
        pnl: 0,
        direction: initialize_data.direction,
    };
    
    let serialized_data = position.try_to_vec().map_err(|_| ProgramError::InvalidAccountData)?;
    let data_len = serialized_data.len();
    
    let seeds = &[
        b"uranus_position",
        owner_account.key.as_ref(),
        &initialize_data.position_nonce.to_le_bytes(),
        &[bump_seed],
    ];
    
    invoke(
        &system_instruction::transfer(
            payer_account.key,
            dex_fees_account.key,
            total_fee,
        ),
        &[
            payer_account.clone(),
            dex_fees_account.clone(),
            system_program.clone(),
        ],
    )?;

    invoke_signed(
        &system_instruction::create_account(
            payer_account.key,
            position_account.key,
            position_amount_after_fees,
            data_len as u64,
            program_id,
        ),
        &[
            payer_account.clone(),
            position_account.clone(),
            system_program.clone(),
        ],
        &[seeds],
    )?;

    position.serialize(&mut *position_account.data.borrow_mut())?;

    msg!("Position initialized: nonce {}", initialize_data.position_nonce);
    msg!("Fee: {} lamports", total_fee);
    msg!("Locked: {} lamports", position_amount_after_fees);
    msg!("Leverage: {}x", leverage);
    msg!("Ticker: {}", fixed_array_to_string(&initialize_data.market_symbol)?);
    msg!("Market mint: {}", initialize_data.market_mint);
    msg!("Direction: {}", if initialize_data.direction == POSITION_LONG { "Long" } else { "Short" });
    msg!("Position size: {}", actual_position_size);
    
    Ok(())
}

fn try_load_position_account(position_account: &AccountInfo) -> Result<PositionAccount, ProgramError> {
    if let Ok(position) = PositionAccount::try_from_slice(&position_account.data.borrow()) {
        return Ok(position);
    }
    
    msg!("Invalid position data");
    msg!("Position account data length: {}", position_account.data.borrow().len());

    Err(ProgramError::InvalidAccountData)
}

fn process_dex_modify(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    dex_data: DexModifyData,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();
    
    let position_account = next_account_info(accounts_iter)?;
    let dex_account = next_account_info(accounts_iter)?;
    
    if !dex_account.is_signer || dex_account.key != &DEX_PUBKEY {
        return Err(ProgramError::MissingRequiredSignature);
    }
    
    if position_account.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    
    let mut position = try_load_position_account(position_account)?;
    
    if position.position_nonce != dex_data.position_nonce {
        return Err(ProgramError::InvalidArgument);
    }
    
    position.entry_price = dex_data.new_entry_price;
    position.liquidation_price = dex_data.new_liquidation_price;
    position.closed = dex_data.new_close_state;
    position.pnl = dex_data.new_pnl;
    position.market_mint = dex_data.new_market_mint;
    
    position.serialize(&mut *position_account.data.borrow_mut())?;
    
    msg!("Position {} updated", position.position_nonce);
    
    Ok(())
}

fn process_user_modify(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    user_data: UserModifyData,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();
    
    let position_account = next_account_info(accounts_iter)?;
    let user_account = next_account_info(accounts_iter)?;
    
    if !user_account.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    
    if position_account.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    
    let mut position = try_load_position_account(position_account)?;
    
    if position.position_nonce != user_data.position_nonce {
        return Err(ProgramError::InvalidArgument);
    }
    
    if position.owner != *user_account.key && user_account.key != &DEX_PUBKEY {
        return Err(ProgramError::InvalidAccountData);
    }
    
    if position.closed != 0 {
        return Err(ProgramError::InvalidAccountData);
    }
    
    if user_data.close_position {
        position.closed = 1;
        msg!("Position {} marked to close", position.position_nonce);
    }
    
    position.serialize(&mut *position_account.data.borrow_mut())?;
    
    Ok(())
}

fn process_pnl(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    pnl_data: ProcessPnlData,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();
    
    let position_account = next_account_info(accounts_iter)?;
    let dex_account = next_account_info(accounts_iter)?;
    let owner_account = next_account_info(accounts_iter)?;
    let market_account = next_account_info(accounts_iter)?;
    let dex_fees_account = next_account_info(accounts_iter)?;
    let system_program = next_account_info(accounts_iter)?;
    
    if !dex_account.is_signer || dex_account.key != &DEX_PUBKEY {
        return Err(ProgramError::MissingRequiredSignature);
    }
    
    if position_account.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    
    let position = try_load_position_account(position_account)?;
    
    if position.position_nonce != pnl_data.position_nonce {
        return Err(ProgramError::InvalidArgument);
    }
    
    if position.closed != 1 {
        return Err(ProgramError::InvalidAccountData);
    }

    if &position.owner != owner_account.key {
        return Err(ProgramError::InvalidArgument);
    }
    
    let (position_pda, _position_bump) = find_position_address(
        &position.owner,
        position.position_nonce,
        program_id
    );
    
    if position_account.key != &position_pda {
        return Err(ProgramError::InvalidArgument);
    }
    
    let (market_liquidity_pda, _market_bump) = find_market_address(
        &position.market_mint,
        program_id
    );
    
    if market_account.key != &market_liquidity_pda {
        msg!("Market account does not match expected PDA");
        return Err(ProgramError::InvalidArgument);
    }

    if market_account.owner != program_id {
        msg!("Market account not owned by program! Owner: {}", market_account.owner);
        return Err(ProgramError::IncorrectProgramId);
    }
    
    let position_lamports = position_account.lamports();
    let market_lamports = market_account.lamports();
    
    msg!("Position lamports: {}", position_lamports);
    msg!("Market lamports: {}", market_lamports);
    
    if pnl_data.final_pnl > 0 {
        let pnl_amount = pnl_data.final_pnl as u64;
        
        let base_fee = pnl_amount.saturating_mul(BASE_FEE_BASIS_POINTS).saturating_div(10000);
        let leverage_fee = pnl_amount
            .saturating_mul(LEVERAGE_FEE_BASIS_POINTS)
            .saturating_mul(position.leverage as u64)
            .saturating_div(10000);
        
        let total_fee = base_fee.saturating_add(leverage_fee);
        let profit_after_fee = pnl_amount.saturating_sub(total_fee);
        let total_required = total_fee.saturating_add(profit_after_fee);
        
        msg!("Required from market: {} lamports", total_required);
        msg!("Market has: {} lamports", market_lamports);
        
        if market_lamports < total_required {
            msg!("Insufficient market liquidity. Required: {}, Available: {}", total_required, market_lamports);
            
            **position_account.lamports.borrow_mut() = position_account
                .lamports()
                .saturating_sub(position_lamports);
            **owner_account.lamports.borrow_mut() = owner_account
                .lamports()
                .saturating_add(position_lamports);
            
            msg!("Market insufficient - returned locked funds only: {}", position_lamports);
        } else {
            if total_fee > 0 {
                **market_account.lamports.borrow_mut() = market_account
                    .lamports()
                    .saturating_sub(total_fee);
                **dex_fees_account.lamports.borrow_mut() = dex_fees_account
                    .lamports()
                    .saturating_add(total_fee);
            }
            
            if profit_after_fee > 0 {
                **market_account.lamports.borrow_mut() = market_account
                    .lamports()
                    .saturating_sub(profit_after_fee);
                **owner_account.lamports.borrow_mut() = owner_account
                    .lamports()
                    .saturating_add(profit_after_fee);
            }
            
            **position_account.lamports.borrow_mut() = position_account
                .lamports()
                .saturating_sub(position_lamports);
            **owner_account.lamports.borrow_mut() = owner_account
                .lamports()
                .saturating_add(position_lamports);
            
            msg!("Profit: {} (fee: {})", profit_after_fee, total_fee);
        }
        
    } else if pnl_data.final_pnl < 0 {
        let pnl_abs = (-pnl_data.final_pnl) as u64;
        
        if position_lamports <= pnl_abs {
            **position_account.lamports.borrow_mut() = position_account
                .lamports()
                .saturating_sub(position_lamports);
            **market_account.lamports.borrow_mut() = market_account
                .lamports()
                .saturating_add(position_lamports);
            
            msg!("Total loss: {} lamports", position_lamports);
        } else {
            let remaining_funds = position_lamports.saturating_sub(pnl_abs);
            
            **position_account.lamports.borrow_mut() = position_account
                .lamports()
                .saturating_sub(pnl_abs);
            **market_account.lamports.borrow_mut() = market_account
                .lamports()
                .saturating_add(pnl_abs);
            
            **position_account.lamports.borrow_mut() = position_account
                .lamports()
                .saturating_sub(remaining_funds);
            **owner_account.lamports.borrow_mut() = owner_account
                .lamports()
                .saturating_add(remaining_funds);
            
            msg!("Loss: {}, remaining: {}", pnl_abs, remaining_funds);
        }
    } else {
        **position_account.lamports.borrow_mut() = position_account
            .lamports()
            .saturating_sub(position_lamports);
        **owner_account.lamports.borrow_mut() = owner_account
            .lamports()
            .saturating_add(position_lamports);
        
        msg!("Zero PnL: {} returned", position_lamports);
    }
    
    zero_account_data(position_account)?;
    
    msg!("Position {} closed", position.position_nonce);
    
    Ok(())
}

fn process_force_close(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();
    
    let position_account = next_account_info(accounts_iter)?;
    let owner_account = next_account_info(accounts_iter)?;
    let dex_account = next_account_info(accounts_iter)?;
    
    if !dex_account.is_signer || dex_account.key != &DEX_PUBKEY {
        return Err(ProgramError::MissingRequiredSignature);
    }
    
    if position_account.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    
    msg!("Force closing corrupted position");
    
    let position_lamports = position_account.lamports();
    **owner_account.lamports.borrow_mut() = owner_account
        .lamports()
        .saturating_add(position_lamports);
    **position_account.lamports.borrow_mut() = 0;
    
    zero_account_data(position_account)?;
    
    msg!("Force closed position, returned {} lamports", position_lamports);
    
    Ok(())
}

fn zero_account_data(account: &AccountInfo) -> ProgramResult {
    let mut data = account.try_borrow_mut_data()?;

    let len = data.len();
    for i in 0..len {
        data[i] = 0;
    }
    Ok(())
}

fn process_market_transfer(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    transfer_data: MarketTransferData,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();
    
    let from_market_account = next_account_info(accounts_iter)?;
    let to_market_account = next_account_info(accounts_iter)?;
    let from_pda = next_account_info(accounts_iter)?;
    let to_pda = next_account_info(accounts_iter)?;
    let dex_account = next_account_info(accounts_iter)?;
    
    if !dex_account.is_signer || dex_account.key != &DEX_PUBKEY {
        msg!("Unauthorized market transfer attempt");
        return Err(ProgramError::MissingRequiredSignature);
    }
    
    let (from_market_pda, from_bump) = find_market_address(
        &transfer_data.from_market_mint,
        program_id
    );
    
    let (to_market_pda, to_bump) = find_market_address(
        &transfer_data.to_market_mint,
        program_id
    );

    if from_pda.key != &from_market_pda {
        msg!("Invalid from_market PDA, expected {}, got {}", from_market_pda, from_pda.key);
        return Err(ProgramError::InvalidArgument);
    }
    
    if to_pda.key != &to_market_pda {
        msg!("Invalid to_market PDA, expected {}, got {}", to_market_pda, to_pda.key);
        return Err(ProgramError::InvalidArgument);
    }
    
    if from_pda.owner != program_id {
        msg!("From market PDA not owned by program");
        return Err(ProgramError::IncorrectProgramId);
    }
    
    if to_pda.owner != program_id {
        msg!("To market PDA not owned by program");
        return Err(ProgramError::IncorrectProgramId);
    }
    
    if !from_pda.data_is_empty() {
        return Err(ProgramError::InvalidAccountData);
    }
    
    if !to_pda.data_is_empty() {
        return Err(ProgramError::InvalidAccountData);
    }
    
    if from_pda.lamports() == 0 {
        return Err(ProgramError::InsufficientFunds);
    }
    
    let from_balance = from_pda.lamports();
    if from_balance < transfer_data.amount {
        msg!("Insufficient balance in from_market PDA. Has: {}, Requested: {}", 
             from_balance, transfer_data.amount);
        return Err(ProgramError::InsufficientFunds);
    }
    
    let rent = Rent::get()?;
    let min_balance = rent.minimum_balance(from_pda.data_len());
    if from_pda.lamports().saturating_sub(transfer_data.amount) < min_balance {
        msg!("Transfer would make from_pda not rent exempt");
        return Err(ProgramError::InsufficientFunds);
    }
    
    if from_pda.key == to_pda.key {
        msg!("Cannot transfer to the same market PDA");
        return Err(ProgramError::InvalidArgument);
    }
    
    **from_pda.lamports.borrow_mut() = from_pda
        .lamports()
        .saturating_sub(transfer_data.amount);
    
    **to_pda.lamports.borrow_mut() = to_pda
        .lamports()
        .saturating_add(transfer_data.amount);
    
    msg!("Market PDA transfer completed:");
    msg!("  From market mint: {}", transfer_data.from_market_mint);
    msg!("  To market mint: {}", transfer_data.to_market_mint);
    msg!("  Amount: {} lamports", transfer_data.amount);
    msg!("  From PDA balance after: {} lamports", from_pda.lamports());
    msg!("  To PDA balance after: {} lamports", to_pda.lamports());
    
    Ok(())
}
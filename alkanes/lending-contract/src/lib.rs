use alkanes_runtime::{declare_alkane, message::MessageDispatch, runtime::AlkaneResponder};

#[allow(unused_imports)]
use alkanes_runtime::{
    println,
    stdio::{stdout, Write},
};
use alkanes_macros::storage_variable;
use alkanes_std_factory_support::MintableToken;
use alkanes_support::{
    id::AlkaneId,
    parcel::AlkaneTransfer,
    response::CallResponse,
};
use anyhow::{anyhow, Result};
use metashrew_support::compat::to_arraybuffer_layout;
use metashrew_support::index_pointer::KeyValuePointer;

/// Lending contract states
/// State 0: Uninitialized
/// State 1: Waiting for creditor to fund (Case 1 - debitor created loan request with collateral)
/// State 2: Waiting for debitor to claim loan (Case 1 - creditor funded, debitor needs to claim)
/// State 3: Loan active (Case 1 - debitor claimed loan, timer started)
/// State 4: Waiting for debitor to take loan (Case 2 - creditor offered loan, waiting for debitor)
/// State 5: Loan active (Case 2 - debitor took loan with collateral)
/// State 6: Loan repaid - closed
/// State 7: Loan defaulted - creditor claimed collateral
const STATE_UNINITIALIZED: u128 = 0;
const STATE_WAITING_FOR_CREDITOR: u128 = 1;
const STATE_WAITING_FOR_DEBITOR_CLAIM: u128 = 2;
const STATE_LOAN_ACTIVE_CASE1: u128 = 3;
const STATE_WAITING_FOR_DEBITOR_TAKE: u128 = 4;
const STATE_LOAN_ACTIVE_CASE2: u128 = 5;
const STATE_LOAN_REPAID: u128 = 6;
const STATE_LOAN_DEFAULTED: u128 = 7;

/// APR precision: 4 decimal places (e.g., 1000 = 10.00%, 500 = 5.00%)
const APR_PRECISION: u128 = 10000;

/// Blocks per year approximation (assuming ~10 min blocks)
/// 6 blocks/hour * 24 hours * 365 days = 52560 blocks/year
const BLOCKS_PER_YEAR: u128 = 52560;

#[derive(MessageDispatch)]
pub enum LendingContractMessage {
    /// Case 1: Debitor creates loan request by depositing collateral
    /// Expects collateral tokens to be sent with this call
    #[opcode(0)]
    InitWithCollateral {
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        duration_blocks: u128,
        desired_apr: u128, // with 4 decimal places of precision
    },

    /// Case 2: Creditor creates loan offer by depositing loan tokens
    /// Expects loan tokens to be sent with this call
    #[opcode(1)]
    InitWithLoanOffer {
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        duration_blocks: u128,
        desired_apr: u128, // with 4 decimal places of precision
    },

    /// Case 1 Step 2: Creditor funds the loan by sending loan tokens
    /// Expects loan tokens to be sent with this call
    #[opcode(2)]
    FundLoan,

    /// Case 1 Step 3: Debitor claims the loan tokens
    /// Transfers loan tokens to debitor and starts the loan duration
    #[opcode(3)]
    ClaimLoan,

    /// Case 2 Step 2: Debitor takes loan by sending collateral
    /// Expects collateral tokens to be sent with this call
    /// Returns loan tokens to debitor immediately
    #[opcode(4)]
    TakeLoanWithCollateral,

    /// Debitor repays the loan (principal + interest)
    /// Expects loan tokens to be sent with this call
    /// Returns collateral to debitor
    #[opcode(5)]
    RepayLoan,

    /// Creditor claims collateral after loan default
    /// Only callable after repayment deadline has passed
    #[opcode(6)]
    ClaimDefaultedCollateral,

    /// Debitor cancels loan request (only before creditor funds)
    /// Returns collateral to debitor
    #[opcode(7)]
    CancelLoanRequest,

    /// Creditor cancels loan offer (only before debitor takes)
    /// Returns loan tokens to creditor
    #[opcode(8)]
    CancelLoanOffer,

    /// Forward incoming tokens (utility)
    #[opcode(50)]
    ForwardIncoming,

    /// Get loan details
    #[opcode(90)]
    GetLoanDetails,

    /// Get current repayment amount (principal + accrued interest)
    #[opcode(91)]
    GetRepaymentAmount,

    /// Get contract state
    #[opcode(92)]
    GetState,

    /// Get time remaining until deadline (in blocks)
    #[opcode(93)]
    GetTimeRemaining,

    /// Get contract name
    #[opcode(99)]
    GetName,

    /// Get contract symbol
    #[opcode(100)]
    GetSymbol,
}

#[derive(Default)]
pub struct LendingContract();

impl MintableToken for LendingContract {}
impl AlkaneResponder for LendingContract {}

impl LendingContract {
    // ============ Storage Variables (using alkanes-macros) ============
    
    // State variable (special naming to avoid conflict with get_state opcode)
    // Returns u128 directly with default of 0 (STATE_UNINITIALIZED)
    storage_variable!(state_value: u128);
    
    // Collateral parameters
    storage_variable!(collateral_token: AlkaneId);
    storage_variable!(collateral_amount: u128);
    
    // Loan parameters
    storage_variable!(loan_token: AlkaneId);
    storage_variable!(loan_amount: u128);
    storage_variable!(duration_blocks: u128);
    storage_variable!(apr: u128);
    
    // Loan timing
    storage_variable!(loan_start_block: u128);
    storage_variable!(repayment_deadline: u128);
    
    // Participants
    storage_variable!(debitor: AlkaneId);
    storage_variable!(creditor: AlkaneId);

    // ============ Helper Functions ============

    fn current_block(&self) -> u128 {
        self.height() as u128
    }

    fn caller(&self) -> Result<AlkaneId> {
        let context = self.context()?;
        Ok(context.caller.clone())
    }

    /// Calculate the total repayment amount (principal + interest)
    /// Interest = principal * apr * (duration_blocks / blocks_per_year) / APR_PRECISION
    fn calculate_repayment_amount(&self) -> Result<u128> {
        let principal = self.loan_amount();
        let apr = self.apr();
        let duration = self.duration_blocks();

        // Interest calculation with precision handling
        // interest = principal * apr * duration / (APR_PRECISION * BLOCKS_PER_YEAR)
        let interest = principal
            .checked_mul(apr)
            .ok_or_else(|| anyhow!("Overflow in interest calculation"))?
            .checked_mul(duration)
            .ok_or_else(|| anyhow!("Overflow in interest calculation"))?
            .checked_div(APR_PRECISION * BLOCKS_PER_YEAR)
            .ok_or_else(|| anyhow!("Division error in interest calculation"))?;

        principal
            .checked_add(interest)
            .ok_or_else(|| anyhow!("Overflow adding interest to principal"))
    }

    /// Validate and collect incoming tokens of a specific type
    fn collect_incoming_tokens(
        &self,
        expected_token: AlkaneId,
        expected_amount: u128,
    ) -> Result<(u128, CallResponse)> {
        let context = self.context()?;
        let mut token_received: u128 = 0;
        let mut response = CallResponse::default();

        for transfer in context.incoming_alkanes.0.clone() {
            if transfer.id == expected_token {
                token_received = token_received
                    .checked_add(transfer.value)
                    .ok_or_else(|| anyhow!("Overflow collecting tokens"))?;
            } else {
                // Refund unexpected tokens
                response.alkanes.pay(transfer);
            }
        }

        if token_received < expected_amount {
            return Err(anyhow!(
                "Insufficient tokens: expected {}, received {}",
                expected_amount,
                token_received
            ));
        }

        // Refund excess tokens
        if token_received > expected_amount {
            response.alkanes.pay(AlkaneTransfer {
                id: expected_token,
                value: token_received - expected_amount,
            });
        }

        Ok((expected_amount, response))
    }

    /// Refund all incoming tokens
    fn refund_all_incoming(&self) -> Result<CallResponse> {
        Ok(CallResponse::forward(&self.context()?.incoming_alkanes))
    }

    // ============ Case 1: Debitor Creates Loan Request ============

    /// Case 1 Step 1: Debitor creates loan request by depositing collateral
    fn init_with_collateral(
        &self,
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        duration_blocks: u128,
        desired_apr: u128,
    ) -> Result<CallResponse> {
        // Ensure contract is not already initialized
        self.observe_initialization()?;

        // Validate inputs
        if collateral_amount == 0 {
            return Err(anyhow!("Collateral amount cannot be zero"));
        }
        if loan_amount == 0 {
            return Err(anyhow!("Loan amount cannot be zero"));
        }
        if duration_blocks == 0 {
            return Err(anyhow!("Duration cannot be zero"));
        }
        if collateral_token == loan_token {
            return Err(anyhow!("Collateral and loan token cannot be the same"));
        }

        // Collect collateral tokens
        let (_, response) = self.collect_incoming_tokens(collateral_token.clone(), collateral_amount)?;

        // Store loan parameters
        self.set_collateral_token(collateral_token);
        self.set_collateral_amount(collateral_amount);
        self.set_loan_token(loan_token);
        self.set_loan_amount(loan_amount);
        self.set_duration_blocks(duration_blocks);
        self.set_apr(desired_apr);
        self.set_debitor(self.caller()?);
        self.set_state_value(STATE_WAITING_FOR_CREDITOR);

        Ok(response)
    }

    /// Case 1 Step 2: Creditor funds the loan
    fn fund_loan(&self) -> Result<CallResponse> {
        let state = self.state_value();
        if state != STATE_WAITING_FOR_CREDITOR {
            return Err(anyhow!("Loan is not in funding state"));
        }

        let loan_token = self.loan_token()?;
        let loan_amount = self.loan_amount();

        // Collect loan tokens from creditor
        let (_, response) = self.collect_incoming_tokens(loan_token, loan_amount)?;

        // Store creditor and update state
        self.set_creditor(self.caller()?);
        self.set_state_value(STATE_WAITING_FOR_DEBITOR_CLAIM);

        Ok(response)
    }

    /// Case 1 Step 3: Debitor claims the loan tokens
    fn claim_loan(&self) -> Result<CallResponse> {
        let state = self.state_value();
        if state != STATE_WAITING_FOR_DEBITOR_CLAIM {
            return Err(anyhow!("Loan is not ready to be claimed"));
        }

        // Verify caller is the debitor
        let debitor = self.debitor()?;
        if self.caller()? != debitor {
            return Err(anyhow!("Only the debitor can claim the loan"));
        }

        let loan_token = self.loan_token()?;
        let loan_amount = self.loan_amount();
        let duration = self.duration_blocks();
        let current_block = self.current_block();

        // Calculate deadline
        let deadline = current_block
            .checked_add(duration)
            .ok_or_else(|| anyhow!("Overflow calculating deadline"))?;

        // Start loan timer
        self.set_loan_start_block(current_block);
        self.set_repayment_deadline(deadline);
        self.set_state_value(STATE_LOAN_ACTIVE_CASE1);

        // Transfer loan tokens to debitor (forward to output)
        let mut response = self.refund_all_incoming()?;
        response.alkanes.pay(AlkaneTransfer {
            id: loan_token,
            value: loan_amount,
        });

        Ok(response)
    }

    // ============ Case 2: Creditor Offers Loan ============

    /// Case 2 Step 1: Creditor creates loan offer by depositing loan tokens
    fn init_with_loan_offer(
        &self,
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        duration_blocks: u128,
        desired_apr: u128,
    ) -> Result<CallResponse> {
        // Ensure contract is not already initialized
        self.observe_initialization()?;

        // Validate inputs
        if collateral_amount == 0 {
            return Err(anyhow!("Collateral amount cannot be zero"));
        }
        if loan_amount == 0 {
            return Err(anyhow!("Loan amount cannot be zero"));
        }
        if duration_blocks == 0 {
            return Err(anyhow!("Duration cannot be zero"));
        }
        if collateral_token == loan_token {
            return Err(anyhow!("Collateral and loan token cannot be the same"));
        }

        // Collect loan tokens from creditor
        let (_, response) = self.collect_incoming_tokens(loan_token.clone(), loan_amount)?;

        // Store loan parameters
        self.set_collateral_token(collateral_token);
        self.set_collateral_amount(collateral_amount);
        self.set_loan_token(loan_token);
        self.set_loan_amount(loan_amount);
        self.set_duration_blocks(duration_blocks);
        self.set_apr(desired_apr);
        self.set_creditor(self.caller()?);
        self.set_state_value(STATE_WAITING_FOR_DEBITOR_TAKE);

        Ok(response)
    }

    /// Case 2 Step 2: Debitor takes loan by providing collateral
    fn take_loan_with_collateral(&self) -> Result<CallResponse> {
        let state = self.state_value();
        if state != STATE_WAITING_FOR_DEBITOR_TAKE {
            return Err(anyhow!("Loan offer is not available"));
        }

        let collateral_token = self.collateral_token()?;
        let collateral_amount = self.collateral_amount();
        let loan_token = self.loan_token()?;
        let loan_amount = self.loan_amount();
        let duration = self.duration_blocks();
        let current_block = self.current_block();

        // Collect collateral from debitor
        let (_, mut response) = self.collect_incoming_tokens(collateral_token, collateral_amount)?;

        // Calculate deadline
        let deadline = current_block
            .checked_add(duration)
            .ok_or_else(|| anyhow!("Overflow calculating deadline"))?;

        // Store debitor and start loan
        self.set_debitor(self.caller()?);
        self.set_loan_start_block(current_block);
        self.set_repayment_deadline(deadline);
        self.set_state_value(STATE_LOAN_ACTIVE_CASE2);

        // Transfer loan tokens to debitor
        response.alkanes.pay(AlkaneTransfer {
            id: loan_token,
            value: loan_amount,
        });

        Ok(response)
    }

    // ============ Loan Lifecycle ============

    /// Repay the loan (principal + interest)
    fn repay_loan(&self) -> Result<CallResponse> {
        let state = self.state_value();
        if state != STATE_LOAN_ACTIVE_CASE1 && state != STATE_LOAN_ACTIVE_CASE2 {
            return Err(anyhow!("No active loan to repay"));
        }

        // Verify caller is the debitor
        let debitor = self.debitor()?;
        if self.caller()? != debitor {
            return Err(anyhow!("Only the debitor can repay the loan"));
        }

        // Check deadline hasn't passed
        let deadline = self.repayment_deadline();
        let current_block = self.current_block();
        if current_block > deadline {
            return Err(anyhow!("Loan has defaulted - deadline passed"));
        }

        let loan_token = self.loan_token()?;
        let repayment_amount = self.calculate_repayment_amount()?;
        let collateral_token = self.collateral_token()?;
        let collateral_amount = self.collateral_amount();
        let _creditor = self.creditor()?;

        // Collect repayment
        let (_, mut response) = self.collect_incoming_tokens(loan_token.clone(), repayment_amount)?;

        // Mark loan as repaid
        self.set_state_value(STATE_LOAN_REPAID);

        // Return collateral to debitor
        response.alkanes.pay(AlkaneTransfer {
            id: collateral_token,
            value: collateral_amount,
        });

        // Send repayment to creditor (we store it in contract, creditor can claim)
        // For simplicity, we'll send the repayment directly
        response.alkanes.pay(AlkaneTransfer {
            id: loan_token,
            value: repayment_amount,
        });

        Ok(response)
    }

    /// Creditor claims collateral after loan default
    fn claim_defaulted_collateral(&self) -> Result<CallResponse> {
        let state = self.state_value();
        if state != STATE_LOAN_ACTIVE_CASE1 && state != STATE_LOAN_ACTIVE_CASE2 {
            return Err(anyhow!("No active loan to claim"));
        }

        // Verify caller is the creditor
        let creditor = self.creditor()?;
        if self.caller()? != creditor {
            return Err(anyhow!("Only the creditor can claim defaulted collateral"));
        }

        // Check deadline has passed
        let deadline = self.repayment_deadline();
        let current_block = self.current_block();
        if current_block <= deadline {
            return Err(anyhow!("Loan has not defaulted yet - deadline not passed"));
        }

        let collateral_token = self.collateral_token()?;
        let collateral_amount = self.collateral_amount();

        // Mark loan as defaulted
        self.set_state_value(STATE_LOAN_DEFAULTED);

        // Transfer collateral to creditor
        let mut response = self.refund_all_incoming()?;
        response.alkanes.pay(AlkaneTransfer {
            id: collateral_token,
            value: collateral_amount,
        });

        Ok(response)
    }

    // ============ Cancellation Functions ============

    /// Debitor cancels loan request (only before creditor funds)
    fn cancel_loan_request(&self) -> Result<CallResponse> {
        let state = self.state_value();
        if state != STATE_WAITING_FOR_CREDITOR {
            return Err(anyhow!("Cannot cancel - loan not in cancellable state"));
        }

        // Verify caller is the debitor
        let debitor = self.debitor()?;
        if self.caller()? != debitor {
            return Err(anyhow!("Only the debitor can cancel the loan request"));
        }

        let collateral_token = self.collateral_token()?;
        let collateral_amount = self.collateral_amount();

        // Return collateral to debitor
        let mut response = self.refund_all_incoming()?;
        response.alkanes.pay(AlkaneTransfer {
            id: collateral_token,
            value: collateral_amount,
        });

        // Reset state (effectively closing the contract)
        self.set_state_value(STATE_UNINITIALIZED);

        Ok(response)
    }

    /// Creditor cancels loan offer (only before debitor takes)
    fn cancel_loan_offer(&self) -> Result<CallResponse> {
        let state = self.state_value();
        if state != STATE_WAITING_FOR_DEBITOR_TAKE {
            return Err(anyhow!("Cannot cancel - loan offer not in cancellable state"));
        }

        // Verify caller is the creditor
        let creditor = self.creditor()?;
        if self.caller()? != creditor {
            return Err(anyhow!("Only the creditor can cancel the loan offer"));
        }

        let loan_token = self.loan_token()?;
        let loan_amount = self.loan_amount();

        // Return loan tokens to creditor
        let mut response = self.refund_all_incoming()?;
        response.alkanes.pay(AlkaneTransfer {
            id: loan_token,
            value: loan_amount,
        });

        // Reset state
        self.set_state_value(STATE_UNINITIALIZED);

        Ok(response)
    }

    // ============ View Functions ============

    fn forward_incoming(&self) -> Result<CallResponse> {
        Ok(CallResponse::forward(&self.context()?.incoming_alkanes))
    }

    /// Get detailed loan information
    fn get_loan_details(&self) -> Result<CallResponse> {
        let context = self.context()?;
        let mut response = CallResponse::forward(&context.incoming_alkanes);

        let state = self.state_value();
        let mut data: Vec<u8> = Vec::new();

        // Encode state
        data.extend_from_slice(&state.to_le_bytes());

        if state != STATE_UNINITIALIZED {
            // Encode collateral token
            let collateral_token = self.collateral_token()?;
            data.extend_from_slice(&collateral_token.block.to_le_bytes());
            data.extend_from_slice(&collateral_token.tx.to_le_bytes());

            // Encode collateral amount
            let collateral_amount = self.collateral_amount();
            data.extend_from_slice(&collateral_amount.to_le_bytes());

            // Encode loan token
            let loan_token = self.loan_token()?;
            data.extend_from_slice(&loan_token.block.to_le_bytes());
            data.extend_from_slice(&loan_token.tx.to_le_bytes());

            // Encode loan amount
            let loan_amount = self.loan_amount();
            data.extend_from_slice(&loan_amount.to_le_bytes());

            // Encode duration
            let duration = self.duration_blocks();
            data.extend_from_slice(&duration.to_le_bytes());

            // Encode APR
            let apr = self.apr();
            data.extend_from_slice(&apr.to_le_bytes());

            // Encode deadline if active
            if state == STATE_LOAN_ACTIVE_CASE1 || state == STATE_LOAN_ACTIVE_CASE2 {
                let deadline = self.repayment_deadline();
                data.extend_from_slice(&deadline.to_le_bytes());

                let start_block = self.loan_start_block();
                data.extend_from_slice(&start_block.to_le_bytes());
            }
        }

        response.data = data;
        Ok(response)
    }

    /// Get current repayment amount
    fn get_repayment_amount(&self) -> Result<CallResponse> {
        let context = self.context()?;
        let mut response = CallResponse::forward(&context.incoming_alkanes);

        let state = self.state_value();
        if state != STATE_LOAN_ACTIVE_CASE1 && state != STATE_LOAN_ACTIVE_CASE2 {
            response.data = 0u128.to_le_bytes().to_vec();
        } else {
            let amount = self.calculate_repayment_amount()?;
            response.data = amount.to_le_bytes().to_vec();
        }

        Ok(response)
    }

    /// Get current state
    fn get_state(&self) -> Result<CallResponse> {
        let context = self.context()?;
        let mut response = CallResponse::forward(&context.incoming_alkanes);
        response.data = self.state_value().to_le_bytes().to_vec();
        Ok(response)
    }

    /// Get time remaining until deadline
    fn get_time_remaining(&self) -> Result<CallResponse> {
        let context = self.context()?;
        let mut response = CallResponse::forward(&context.incoming_alkanes);

        let state = self.state_value();
        if state != STATE_LOAN_ACTIVE_CASE1 && state != STATE_LOAN_ACTIVE_CASE2 {
            response.data = 0u128.to_le_bytes().to_vec();
        } else {
            let deadline = self.repayment_deadline();
            let current_block = self.current_block();
            if current_block >= deadline {
                response.data = 0u128.to_le_bytes().to_vec();
            } else {
                let remaining = deadline - current_block;
                response.data = remaining.to_le_bytes().to_vec();
            }
        }

        Ok(response)
    }

    /// Get token name
    fn get_name(&self) -> Result<CallResponse> {
        let context = self.context()?;
        let mut response = CallResponse::forward(&context.incoming_alkanes);
        response.data = self.name().into_bytes().to_vec();
        Ok(response)
    }

    /// Get token symbol
    fn get_symbol(&self) -> Result<CallResponse> {
        let context = self.context()?;
        let mut response = CallResponse::forward(&context.incoming_alkanes);
        response.data = self.symbol().into_bytes().to_vec();
        Ok(response)
    }
}

declare_alkane! {
    impl AlkaneResponder for LendingContract {
        type Message = LendingContractMessage;
    }
}

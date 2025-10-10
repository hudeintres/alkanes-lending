use alkanes_runtime::{
    declare_alkane, message::MessageDispatch, runtime::AlkaneResponder, storage::StoragePointer,
};

#[allow(unused_imports)]
use alkanes_runtime::{
    println,
    stdio::{stdout, Write},
};
use alkanes_std_factory_support::MintableToken;
use alkanes_support::{
    cellpack::Cellpack,
    checked_expr,
    context::Context,
    id::AlkaneId,
    parcel::{AlkaneTransfer, AlkaneTransferParcel},
    response::CallResponse,
    utils::{overflow_error, shift, shift_or_err},
};
use anyhow::{anyhow, Result};
use bitcoin::Block;
use metashrew_support::compat::to_arraybuffer_layout;
use metashrew_support::{byte_view::ByteView, index_pointer::KeyValuePointer, utils::consume_u128};
use protorune_support::balance_sheet::{BalanceSheetOperations, CachedBalanceSheet};
use protorune_support::utils::consensus_decode;
use std::{cmp::min, sync::Arc};

#[derive(MessageDispatch)]
pub enum LendingContractMessage {
    #[opcode(0)]
    InitWithLoanAmount {
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        repayment_block_deadline: u128,
        desired_apr: u128, // with 4 decimal places of precision
    },
    #[opcode(1)]
    InitWithCollateralAmount {
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        repayment_block_deadline: u128,
        desired_apr: u128, // with 4 decimal places of precision
    },
    #[opcode(50)]
    ForwardIncoming,

    #[opcode(99)]
    #[returns(String)]
    GetName,
}

#[derive(Default)]
pub struct LendingContract();

impl MintableToken for LendingContract {}
impl AlkaneResponder for LendingContract {}

impl LendingContract {
    fn collateral_token_pointer(&self) -> StoragePointer {
        StoragePointer::from_keyword("/collateral_token")
    }
    fn collateral_token(&self) -> Result<AlkaneId> {
        let ptr = self.collateral_token_pointer().get().as_ref().clone();
        let mut cursor = std::io::Cursor::<Vec<u8>>::new(ptr);
        Ok(AlkaneId::new(
            consume_u128(&mut cursor)?,
            consume_u128(&mut cursor)?,
        ))
    }
    fn set_collateral_token(&self, collateral_token_id: AlkaneId) {
        let mut ptr = self.collateral_token_pointer();
        ptr.set(Arc::new(collateral_token_id.into()));
    }

    fn _init_state(
        &self,
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        repayment_block_deadline: u128,
        desired_apr: u128, // with 4 decimal places of precision
    ) -> Result<()> {
        self.observe_initialization()?;
        self.set_collateral_token(collateral_token);
        Ok(())
    }

    fn _refund_and_check_inputs(
        &self,
        desired_input_token: AlkaneId,
        desired_input_amount: u128,
    ) -> Result<CallResponse> {
        let context = self.context()?;
        let mut token_received: u128 = 0;
        let mut ret = CallResponse::default();
        for alkane_transfer in context.incoming_alkanes.0.clone() {
            if alkane_transfer.id != desired_input_token {
                ret.alkanes.pay(alkane_transfer);
            } else {
                token_received += alkane_transfer.value;
            }
        }
        if desired_input_amount > token_received {
            return Err(anyhow!(format!(
                "desired amount ({}) is greater than amount input ({})",
                desired_input_amount, token_received
            )));
        }
        ret.alkanes.pay(AlkaneTransfer {
            id: desired_input_token,
            value: token_received - desired_input_amount,
        });
        Ok(ret)
    }

    fn init_with_loan_amount(
        &self,
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        repayment_block_deadline: u128,
        desired_apr: u128, // with 4 decimal places of precision
    ) -> Result<CallResponse> {
        self._init_state(
            collateral_token,
            collateral_amount,
            loan_token,
            loan_amount,
            repayment_block_deadline,
            desired_apr,
        )?;
        self._refund_and_check_inputs(loan_token, loan_amount)
    }

    fn init_with_collateral_amount(
        &self,
        collateral_token: AlkaneId,
        collateral_amount: u128,
        loan_token: AlkaneId,
        loan_amount: u128,
        repayment_block_deadline: u128,
        desired_apr: u128, // with 4 decimal places of precision
    ) -> Result<CallResponse> {
        self._init_state(
            collateral_token,
            collateral_amount,
            loan_token,
            loan_amount,
            repayment_block_deadline,
            desired_apr,
        )?;
        self._refund_and_check_inputs(collateral_token, collateral_amount)
    }

    fn forward_incoming(&self) -> Result<CallResponse> {
        Ok(CallResponse::forward(&self.context()?.incoming_alkanes))
    }

    /// Get token name
    fn get_name(&self) -> Result<CallResponse> {
        let context = self.context()?;
        let mut response: CallResponse = CallResponse::forward(&context.incoming_alkanes);
        response.data = self.name().into_bytes().to_vec();
        Ok(response)
    }

    /// Get token symbol
    fn get_symbol(&self) -> Result<CallResponse> {
        let context = self.context()?;
        let mut response: CallResponse = CallResponse::forward(&context.incoming_alkanes);
        response.data = self.symbol().into_bytes().to_vec();
        Ok(response)
    }
}

declare_alkane! {
    impl AlkaneResponder for LendingContract {
        type Message = LendingContractMessage;
    }
}

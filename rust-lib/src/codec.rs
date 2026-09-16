//! ABI encoding for ERC-20 transfers and one Multicall3 balance batch.

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol;
use alloy::sol_types::SolCall;

sol! {
    interface IERC20 {
        function balanceOf(address owner) external view returns (uint256);
        function transfer(address to, uint256 amount) external returns (bool);
    }
    struct Call3 { address target; bool allowFailure; bytes callData; }
    struct Result3 { bool success; bytes returnData; }
    interface IMulticall3 {
        function aggregate3(Call3[] calls) external payable returns (Result3[] returnData);
        function getEthBalance(address addr) external view returns (uint256);
    }
}

pub const MULTICALL3: &str = "0xcA11bde05977b3631167028862bE2a173976CA11";

pub fn multicall3() -> Address {
    MULTICALL3.parse().expect("canonical address")
}
pub fn balance_of(owner: Address) -> Vec<u8> {
    IERC20::balanceOfCall { owner }.abi_encode()
}
pub fn transfer(to: Address, amount: U256) -> Vec<u8> {
    IERC20::transferCall { to, amount }.abi_encode()
}
pub fn native_balance(owner: Address) -> Vec<u8> {
    IMulticall3::getEthBalanceCall { addr: owner }.abi_encode()
}
pub fn aggregate(calls: &[(Address, Vec<u8>)]) -> Vec<u8> {
    let calls = calls
        .iter()
        .map(|(target, data)| Call3 {
            target: *target,
            allowFailure: true,
            callData: Bytes::from(data.clone()),
        })
        .collect();
    IMulticall3::aggregate3Call { calls }.abi_encode()
}
pub fn decode_aggregate(data: &[u8]) -> Option<Vec<Option<U256>>> {
    let decoded = IMulticall3::aggregate3Call::abi_decode_returns(data).ok()?;
    Some(
        decoded
            .into_iter()
            .map(|row| {
                (row.success && row.returnData.len() >= 32)
                    .then(|| U256::from_be_slice(&row.returnData[..32]))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    const ALICE: Address = address!("70997970C51812dc3A010C7d01b50e0d17dc79C8");

    #[test]
    fn transfer_has_the_erc20_selector() {
        assert_eq!(
            &transfer(ALICE, U256::from(7))[..4],
            &[0xa9, 0x05, 0x9c, 0xbb]
        );
    }

    #[test]
    fn balance_has_the_erc20_selector() {
        assert_eq!(&balance_of(ALICE)[..4], &[0x70, 0xa0, 0x82, 0x31]);
    }

    #[test]
    fn aggregate_allows_each_leg_to_fail() {
        let encoded = aggregate(&[(ALICE, balance_of(ALICE))]);
        assert_eq!(&encoded[..4], &[0x82, 0xad, 0x56, 0xcb]);
    }
}

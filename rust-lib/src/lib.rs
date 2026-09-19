//! Reusable EVM asset composition. This module reads chain facts from eth_rpc_module and never
//! configures it; its caller names the ERC-20 tokens. It never signs, requests approval, or
//! broadcasts.

pub mod assets;
pub mod budget;
pub mod codec;
pub mod rows;
pub mod transfer;
pub mod units;
pub mod verified;

#[cfg(feature = "logos_module")]
mod glue;

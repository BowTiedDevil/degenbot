#![expect(clippy::unwrap_used)]
// Opcode-format golden spec for the `enc_*` primitive builders.
//
// The byte literals in this file ARE the authoritative definition of each
// opcode's wire format — they are not derived from an external oracle
// (deriving them from the `enc_*` functions under test would be circular).
// Each assertion pins a primitive's exact output so a format change is a
// visible, reviewable diff here.
#![allow(
    clippy::too_many_lines,
    clippy::expect_used,
    clippy::unreadable_literal
)]

use alloy::primitives::{address, U256};
use degenbot_executor::encoders::{self, V4BatchEntry};

#[test]
fn opcode_formats_are_stable() {
    // The shared address table for enc_set_addresses / enc_preamble cases.
    let table = encoders::AddressTable::with_sentinels(
        Some(address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")),
        Some(address!("DeAd0000000000000000000000000000000000Be")),
        Some(address!("000000000004444c5dc75cB358380D2e3dE08A90")),
    );
    let mut table = table;
    let _usdc = table
        .add(address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"))
        .unwrap();
    let _wbtc = table
        .add(address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"))
        .unwrap();
    let _native = table
        .add(address!("0000000000000000000000000000000000000000"))
        .unwrap(); // sentinel, not added

    assert_eq!(
        encoders::enc_set_address(address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")),
        b"\x00\xc0\x2a\xaa\x39\xb2\x23\xfe\x8d\x0a\x0e\x5c\x4f\x27\xea\xd9\x08\x3c\x75\x6c\xc2"
            .to_vec()
    ); // enc_set_address
    assert_eq!(
        encoders::enc_set_address(address!("DeAd0000000000000000000000000000000000Be")),
        b"\x00\xde\xad\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xbe"
            .to_vec()
    ); // enc_set_address
    assert_eq!(encoders::enc_set_addresses(&table), b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x22\x60\xfa\xc5\xe5\x54\x2a\x77\x3a\xa4\x4f\xbc\xfe\xdf\x7c\x19\x3b\xc2\xc5\x99".to_vec()); // enc_set_addresses
    assert_eq!(encoders::enc_preamble(&table), b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x22\x60\xfa\xc5\xe5\x54\x2a\x77\x3a\xa4\x4f\xbc\xfe\xdf\x7c\x19\x3b\xc2\xc5\x99\xff".to_vec()); // enc_preamble
    assert_eq!(
        encoders::enc_erc20_transfer(1u8, 2u8, 1000000000000000000u128).unwrap(),
        b"\x10\x01\x02\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00".to_vec()
    ); // enc_erc20_transfer
    assert_eq!(
        encoders::enc_erc20_transfer(3u8, 0u8, 79228162514264337593543950335u128).unwrap(),
        b"\x10\x03\x00\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff".to_vec()
    ); // enc_erc20_transfer
    assert_eq!(
        encoders::enc_erc20_transfer(0u8, 0u8, 0u128).unwrap(),
        b"\x10\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()
    ); // enc_erc20_transfer
    assert_eq!(
        encoders::enc_erc20_xfer_balance(1u8, 2u8),
        b"\x11\x01\x02".to_vec()
    ); // enc_erc20_xfer_balance
    assert_eq!(encoders::enc_weth_deposit(U256::from(1000000000000000000u128)), b"\x12\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00".to_vec()); // enc_weth_deposit
    assert_eq!(encoders::enc_weth_deposit(U256::from(0u128)), b"\x12\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()); // enc_weth_deposit
    assert_eq!(encoders::enc_weth_withdraw(U256::from(2000000000u128)), b"\x13\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00".to_vec()); // enc_weth_withdraw
    assert_eq!(encoders::enc_weth_deposit_all(), b"\x14".to_vec()); // enc_weth_deposit_all
    assert_eq!(encoders::enc_weth_withdraw_all(), b"\x15".to_vec()); // enc_weth_withdraw_all
    assert_eq!(
        encoders::enc_send_eth(2u8, 1000000000000000000u128).unwrap(),
        b"\x16\x02\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00".to_vec()
    ); // enc_send_eth
    assert_eq!(
        encoders::enc_send_eth(0u8, 311917102708983781990730508u128).unwrap(),
        b"\x16\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c".to_vec()
    ); // enc_send_eth
    assert_eq!(encoders::enc_send_eth_all(2u8), b"\x17\x02".to_vec()); // enc_send_eth_all
    assert_eq!(
        encoders::enc_v2_swap_compact(1u8, true, 1000000000000000000u128, 2u8, 30u16, b"").unwrap(),
        b"\x20\x01\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x02\x00\x1e\x00".to_vec()
    ); // enc_v2_swap_compact
    assert_eq!(
        encoders::enc_v2_swap_compact(2u8, false, 2000000000u128, 0u8, 25u16, b"\x11\x22\x33")
            .unwrap(),
        b"\x20\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x00\x00\x19\x03\x11\x22\x33"
            .to_vec()
    ); // enc_v2_swap_compact
    assert_eq!(
        encoders::enc_v2_swap_compact(
            3u8,
            true,
            79228162514264337593543950335u128,
            1u8,
            100u16,
            b""
        )
        .unwrap(),
        b"\x20\x03\x01\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\x01\x00\x64\x00".to_vec()
    ); // enc_v2_swap_compact
    assert_eq!(
        encoders::enc_v2_swap_calc(1u8, true, 2u8, 30u16),
        b"\x21\x01\x01\x02\x00\x1e".to_vec()
    ); // enc_v2_swap_calc
    assert_eq!(
        encoders::enc_v2_swap_calc(2u8, false, 0u8, 25u16),
        b"\x21\x02\x00\x00\x00\x19".to_vec()
    ); // enc_v2_swap_calc
    assert_eq!(
        encoders::enc_v2_swap_direct(1u8, true, 1000000000000000000u128, 2u8).unwrap(),
        b"\x22\x01\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x02".to_vec()
    ); // enc_v2_swap_direct
    assert_eq!(
        encoders::enc_v2_swap_direct(2u8, false, 311917102708983781990730508u128, 0u8).unwrap(),
        b"\x22\x02\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x00".to_vec()
    ); // enc_v2_swap_direct
    assert_eq!(
        encoders::enc_v3_swap_compact(1u8, true, 1000000000000000000u128, 2u8, b"").unwrap(),
        b"\x30\x01\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x02\x00".to_vec()
    ); // enc_v3_swap_compact
    assert_eq!(
        encoders::enc_v3_swap_compact(2u8, false, 2000000000u128, 0u8, b"\xaa\xbb\xcc\xdd")
            .unwrap(),
        b"\x30\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x00\x04\xaa\xbb\xcc\xdd"
            .to_vec()
    ); // enc_v3_swap_compact
    assert_eq!(
        encoders::enc_v3_swap_delta(1u8, true, 2u8),
        b"\x31\x01\x01\x02".to_vec()
    ); // enc_v3_swap_delta
    assert_eq!(
        encoders::enc_v3_swap_delta(2u8, false, 0u8),
        b"\x31\x02\x00\x00".to_vec()
    ); // enc_v3_swap_delta
    assert_eq!(
        encoders::enc_v4_swap_compact(
            1u8,
            2u8,
            3000u16,
            60i16,
            0xffu8,
            true,
            1000000000000000000u128
        )
        .unwrap(),
        b"\x40\x01\x02\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00"
            .to_vec()
    ); // enc_v4_swap_compact
    assert_eq!(
        encoders::enc_v4_swap_compact(
            3u8,
            4u8,
            500u16,
            10i16,
            0xffu8,
            false,
            79228162514264337593543950335u128
        )
        .unwrap(),
        b"\x40\x03\x04\x01\xf4\x00\x0a\xff\x00\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff"
            .to_vec()
    ); // enc_v4_swap_compact
    assert_eq!(
        encoders::enc_v4_swap_compact(1u8, 2u8, 100u16, -1i16, 0xffu8, true, 7u128).unwrap(),
        b"\x40\x01\x02\x00\x64\xff\xff\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x07"
            .to_vec()
    ); // enc_v4_swap_compact
    assert_eq!(
        encoders::enc_v4_swap_dynamic(1u8, 2u8, 3000u16, 60i16, 0xffu8, true),
        b"\x41\x01\x02\x0b\xb8\x00\x3c\xff\x01".to_vec()
    ); // enc_v4_swap_dynamic
    assert_eq!(
        encoders::enc_v4_swap_dynamic(3u8, 4u8, 500u16, 10i16, 0xffu8, false),
        b"\x41\x03\x04\x01\xf4\x00\x0a\xff\x00".to_vec()
    ); // enc_v4_swap_dynamic
    assert_eq!(encoders::enc_v4_batch(&[V4BatchEntry{c0_idx:1u8,c1_idx:2u8,fee:3000u16,tick_spacing:60i16,hooks_idx:0xffu8,zfo:true,amount_u96:1000000000000000000u128},V4BatchEntry{c0_idx:2u8,c1_idx:1u8,fee:500u16,tick_spacing:10i16,hooks_idx:0xffu8,zfo:false,amount_u96:0u128}]).unwrap(), b"\x42\x02\x01\x02\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x02\x01\x01\xf4\x00\x0a\xff\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()); // enc_v4_batch
    assert_eq!(
        encoders::enc_v4_batch(&[V4BatchEntry {
            c0_idx: 5u8,
            c1_idx: 6u8,
            fee: 10000u16,
            tick_spacing: 200i16,
            hooks_idx: 0xffu8,
            zfo: true,
            amount_u96: 79228162514264337593543950335u128
        }])
        .unwrap(),
        b"\x42\x01\x05\x06\x27\x10\x00\xc8\xff\x01\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff"
            .to_vec()
    ); // enc_v4_batch
    assert_eq!(
        encoders::enc_v4_unlock(b"\x40\x01\x02\x10\x01\x02").unwrap(),
        b"\x50\x06\x40\x01\x02\x10\x01\x02".to_vec()
    ); // enc_v4_unlock
    assert_eq!(encoders::enc_v4_unlock(b"").unwrap(), b"\x50\x00".to_vec()); // enc_v4_unlock
    assert_eq!(encoders::enc_v4_take(1u8, 2u8, U256::from(1000000000000000000u128)), b"\x51\x01\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00".to_vec()); // enc_v4_take
    assert_eq!(encoders::enc_v4_take(3u8, 0u8, U256::from(0u128)), b"\x51\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()); // enc_v4_take
    assert_eq!(
        encoders::enc_v4_take_compact(1u8, 2u8, 1000000000000000000u128).unwrap(),
        b"\x52\x01\x02\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00".to_vec()
    ); // enc_v4_take_compact
    assert_eq!(
        encoders::enc_v4_take_compact(3u8, 0u8, 79228162514264337593543950335u128).unwrap(),
        b"\x52\x03\x00\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff".to_vec()
    ); // enc_v4_take_compact
    assert_eq!(
        encoders::enc_v4_take_delta(1u8, 2u8),
        b"\x53\x01\x02".to_vec()
    ); // enc_v4_take_delta
    assert_eq!(encoders::enc_v4_sync(1u8), b"\x54\x01".to_vec()); // enc_v4_sync
    assert_eq!(encoders::enc_v4_settle(), b"\x55".to_vec()); // enc_v4_settle
    assert_eq!(encoders::enc_v4_settle_delta(1u8), b"\x56\x01".to_vec()); // enc_v4_settle_delta
    assert_eq!(encoders::enc_v4_settle_all(), b"\x57".to_vec()); // enc_v4_settle_all
    assert_eq!(
        encoders::enc_v4_mint_compact(1u8, 2u8, 1000000000000000000u128).unwrap(),
        b"\x58\x01\x02\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00".to_vec()
    ); // enc_v4_mint_compact
    assert_eq!(
        encoders::enc_v4_burn_compact(1u8, 79228162514264337593543950335u128).unwrap(),
        b"\x59\x01\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff".to_vec()
    ); // enc_v4_burn_compact
    assert_eq!(encoders::pack_config(0u8, U256::from(0u128), 0u16, 0u8).unwrap().to_be_bytes::<32>().to_vec(), b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()); // pack_config
    assert_eq!(encoders::pack_config(1u8, U256::from(1000000000000000000u128), 500u16, 2u8).unwrap().to_be_bytes::<32>().to_vec(), b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x02\x01\xf4\x01".to_vec()); // pack_config
    assert_eq!(encoders::pack_config(2u8, U256::from(792281625142643375935439503350u128), 10000u16, 31u8).unwrap().to_be_bytes::<32>().to_vec(), b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x09\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xf6\x1f\x27\x10\x02".to_vec()); // pack_config
    assert_eq!(encoders::pack_expected_balance(1u8, U256::from(1000000000000000000u128)).unwrap().to_be_bytes::<32>().to_vec(), b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x00\x00\x00\x01".to_vec()); // pack_expected_balance
    assert_eq!(pool_key_to_canon(&encoders::make_pool_key(address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"), 3000u32, 60i32, address!("0000000000000000000000000000000000000000"))), b"\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xc0\x2a\xaa\x39\xb2\x23\xfe\x8d\x0a\x0e\x5c\x4f\x27\xea\xd9\x08\x3c\x75\x6c\xc2\x00\x00\x0b\xb8\x00\x00\x00\x3c\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()); // make_pool_key
    assert_eq!(pool_key_to_canon(&encoders::make_pool_key(address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"), address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), 500u32, 10i32, address!("0000000000000000000000000000000000000000"))), b"\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xc0\x2a\xaa\x39\xb2\x23\xfe\x8d\x0a\x0e\x5c\x4f\x27\xea\xd9\x08\x3c\x75\x6c\xc2\x00\x00\x01\xf4\x00\x00\x00\x0a\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()); // make_pool_key
    assert_eq!(pool_key_to_canon(&encoders::make_pool_key(address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"), address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), 100u32, -60i32, address!("DeAd0000000000000000000000000000000000Be"))), b"\x22\x60\xfa\xc5\xe5\x54\x2a\x77\x3a\xa4\x4f\xbc\xfe\xdf\x7c\x19\x3b\xc2\xc5\x99\xc0\x2a\xaa\x39\xb2\x23\xfe\x8d\x0a\x0e\x5c\x4f\x27\xea\xd9\x08\x3c\x75\x6c\xc2\x00\x00\x00\x64\xff\xff\xff\xc4\xde\xad\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xbe".to_vec());
    // make_pool_key
}

/// Encode a `V4PoolKey` into the 68-byte canon form the oracle packs:
/// `[currency0:20][currency1:20][fee:4][tick_spacing:4][hooks:20]`.
fn pool_key_to_canon(k: &encoders::V4PoolKey) -> Vec<u8> {
    let mut out = Vec::with_capacity(68);
    out.extend_from_slice(k.currency0.as_slice());
    out.extend_from_slice(k.currency1.as_slice());
    out.extend_from_slice(&k.fee.to_be_bytes());
    out.extend_from_slice(&k.tick_spacing.to_be_bytes());
    out.extend_from_slice(k.hooks.as_slice());
    out
}

use alloy_primitives::{
    Address,
    aliases::{I24, I256, U160, U256},
    hex, keccak256, uint,
};
use num_bigint::BigUint;
use pyo3::{
    prelude::*,
    types::{PyBytes, PyString},
};
use rayon::prelude::*;

uint! {
    const MIN_SQRT_RATIO: U160 = 4295128739_U160;
    const MAX_SQRT_RATIO: U160 = 1461446703485210103287273052203988822378723970342_U160;
}

#[pyfunction]
pub fn get_sqrt_ratio_at_tick_alloy_translator(tick: i32) -> PyResult<BigUint> {
    let result = get_sqrt_ratio_at_tick_alloy(tick);
    let result = result.to_be_bytes::<20>();
    let result = BigUint::from_bytes_be(&result);
    Ok(result)
}

pub fn get_sqrt_ratio_at_tick_alloy(tick: i32) -> U160 {
    let abs_tick: U256 = U256::try_from(tick.unsigned_abs()).unwrap();

    let zero: U256 = U256::ZERO;
    let one: U256 = U256::ONE;

    let mut ratio: U256 = if abs_tick & one != zero {
        uint!(0xfffcb933bd6fad37aa2d162d1a594001_U256)
    } else {
        uint!(0x100000000000000000000000000000000_U256)
    };

    for (tick_mask, ratio_multiplier) in uint!([
        (0x2_U256, 0xfff97272373d413259a46990580e213a_U256),
        (0x4_U256, 0xfff2e50f5f656932ef12357cf3c7fdcc_U256),
        (0x8_U256, 0xffe5caca7e10e4e61c3624eaa0941cd0_U256),
        (0x10_U256, 0xffcb9843d60f6159c9db58835c926644_U256),
        (0x20_U256, 0xff973b41fa98c081472e6896dfb254c0_U256),
        (0x40_U256, 0xff2ea16466c96a3843ec78b326b52861_U256),
        (0x80_U256, 0xfe5dee046a99a2a811c461f1969c3053_U256),
        (0x100_U256, 0xfcbe86c7900a88aedcffc83b479aa3a4_U256),
        (0x200_U256, 0xf987a7253ac413176f2b074cf7815e54_U256),
        (0x400_U256, 0xf3392b0822b70005940c7a398e4b70f3_U256),
        (0x800_U256, 0xe7159475a2c29b7443b29c7fa6e889d9_U256),
        (0x1000_U256, 0xd097f3bdfd2022b8845ad8f792aa5825_U256),
        (0x2000_U256, 0xa9f746462d870fdf8a65dc1f90e061e5_U256),
        (0x4000_U256, 0x70d869a156d2a1b890bb3df62baf32f7_U256),
        (0x8000_U256, 0x31be135f97d08fd981231505542fcfa6_U256),
        (0x10000_U256, 0x9aa508b5b7a84e1c677de54f3e99bc9_U256),
        (0x20000_U256, 0x5d6af8dedb81196699c329225ee604_U256),
        (0x40000_U256, 0x2216e584f5fa1ea926041bedfe98_U256),
        (0x80000_U256, 0x48a170391f7dc42444e8fa2_U256),
    ]) {
        if (abs_tick & tick_mask) != zero {
            ratio = (ratio * ratio_multiplier) >> 128;
        }
    }

    if tick > 0 {
        ratio = U256::MAX / ratio;
    }

    let mut sqrt_ratio: U256 = ratio >> 32;
    if (ratio % (one << 32)) != zero {
        sqrt_ratio += one
    }

    sqrt_ratio.to::<U160>()
}

#[pyfunction]
pub fn get_tick_at_sqrt_ratio_alloy_translator(sqrt_price_x96: BigUint) -> PyResult<i32> {
    let mut sqrt_price_x96 = sqrt_price_x96.to_bytes_le();

    // pad the vec with zeros until it meets the expected length (20 bytes)
    loop {
        if sqrt_price_x96.len() < 20 {
            sqrt_price_x96.push(0);
        } else {
            break;
        }
    }

    let sqrt_price_x96: [u8; 20] = sqrt_price_x96.try_into().unwrap();
    let sqrt_price_x96: U160 = U160::from_le_bytes(sqrt_price_x96.into());
    Ok(get_tick_at_sqrt_ratio_alloy(sqrt_price_x96).as_i32())
}

pub fn get_tick_at_sqrt_ratio_alloy(sqrt_price_x96: U160) -> I24 {
    assert!(sqrt_price_x96 >= MIN_SQRT_RATIO && sqrt_price_x96 < MAX_SQRT_RATIO);

    let ratio: U256 = U256::from(sqrt_price_x96) << 32;
    let mut r: U256 = ratio;
    let mut msb: U256 = U256::ZERO;
    let mut f: U256;

    let factor_and_shift_values: [(u128, u8); 8] = [
        (0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF, 7),
        (0xFFFFFFFFFFFFFFFF, 6),
        (0xFFFFFFFF, 5),
        (0xFFFF, 4),
        (0xFF, 3),
        (0xF, 2),
        (0x3, 1),
        (0x1, 0),
    ];
    for (factor, shift_bits) in factor_and_shift_values {
        f = (if r > factor { U256::ONE } else { U256::ZERO }) << shift_bits;
        msb |= f;
        r >>= f;
    }

    let mut log_2: I256;
    {
        // shadow "msb" inside this block
        let msb: usize = msb.to();

        r = if msb >= 128 {
            ratio >> (msb - 127)
        } else {
            ratio << (127 - msb)
        };

        log_2 = (I256::unchecked_from(msb) - I256::unchecked_from(128))
            .asl(64)
            .unwrap();
    };

    for shift_factor in (51..=63).rev() {
        r = (r * r) >> 127;
        f = r >> 128;
        log_2 |= I256::unchecked_from(f) << shift_factor;
        r >>= f;
    }

    r = (r * r) >> 127;
    f = r >> 128;
    log_2 |= I256::unchecked_from(f) << 50;

    let log_sqrt10001: I256 = log_2 * I256::unchecked_from("255738958999603826347141");

    // TODO: investigate why .asr(128) works, but >> 128 fails
    let tick_low: I24 = I24::from(
        (log_sqrt10001 - I256::unchecked_from("3402992956809132418596140100660247210")).asr(128),
    );
    let tick_high: I24 = I24::from(
        (log_sqrt10001 + I256::unchecked_from("291339464771989622907027621153398088495")).asr(128),
    );

    if tick_low == tick_high {
        tick_low
    } else {
        if get_sqrt_ratio_at_tick_alloy(tick_high.as_i32()) <= sqrt_price_x96.to::<U160>() {
            tick_high
        } else {
            tick_low
        }
    }
}

pub fn to_checksum_address_alloy(address: &str) -> String {
    address.parse::<Address>().unwrap().to_string()
}

pub fn to_checksum_address_native(address: &str) -> String {
    let address: String = address.strip_prefix("0x").unwrap_or(address).to_lowercase();
    let hex_hash: String = hex::encode(keccak256(address.as_bytes()));

    let mut checksummed_address = String::with_capacity(42);
    checksummed_address.push_str("0x");

    for (addr_char, hash_char) in address.chars().zip(hex_hash.chars()) {
        checksummed_address.push(
            if addr_char.is_alphabetic() && hash_char.to_digit(16).unwrap() > 7 {
                addr_char.to_ascii_uppercase()
            } else {
                addr_char
            },
        )
    }

    checksummed_address
}

#[pyfunction]
pub fn to_checksum_address(address: Bound<'_, PyAny>) -> String {
    let mut _address: String = String::with_capacity(40);

    if address.is_instance_of::<PyString>() {
        to_checksum_address_alloy(address.extract().unwrap())
    } else if address.is_instance_of::<PyBytes>() {
        let address_bytes: [u8; 20] = address.extract().unwrap();
        let address_hex: String = address_bytes
            .iter()
            .map(|b: &u8| format!("{:02x}", b))
            .collect();
        to_checksum_address_alloy(&address_hex)
    } else {
        panic!("Address must be string or bytes")
    }
}

#[pyfunction]
pub fn to_checksum_addresses_parallel(addresses: Bound<'_, PyAny>) -> Vec<String> {
    let _addresses = addresses.extract::<Vec<[u8; 20]>>().unwrap();

    let checksummed: Vec<String> = _addresses
        .par_iter()
        .map(|address| {
            let address_hex: String = address.iter().map(|b: &u8| format!("{:02x}", b)).collect();
            to_checksum_address_alloy(&address_hex)
        })
        .collect();

    checksummed
}

#[pyfunction]
pub fn to_checksum_addresses_sequential(addresses: Bound<'_, PyAny>) -> Vec<String> {
    let _addresses = addresses.extract::<Vec<[u8; 20]>>().unwrap();

    let checksummed: Vec<String> = _addresses
        .iter()
        .map(|addr| {
            let address_hex: String = addr.iter().map(|b: &u8| format!("{:02x}", b)).collect();
            to_checksum_address_alloy(&address_hex)
        })
        .collect();

    checksummed
}

#[pymodule]
fn degenbot_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(get_sqrt_ratio_at_tick_alloy_translator, m).unwrap())
        .unwrap();
    m.add_function(wrap_pyfunction!(get_tick_at_sqrt_ratio_alloy_translator, m).unwrap())
        .unwrap();

    m.add_function(wrap_pyfunction!(to_checksum_address, m).unwrap())
        .unwrap();
    m.add_function(wrap_pyfunction!(to_checksum_addresses_parallel, m).unwrap())
        .unwrap();
    m.add_function(wrap_pyfunction!(to_checksum_addresses_sequential, m).unwrap())
        .unwrap();

    Ok(())
}

#[test]
fn test_tickmath() -> () {
    use std::str::FromStr;

    const MIN_TICK: i32 = -887272;
    const MAX_TICK: i32 = 887272;

    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(MIN_TICK),
        U160::from_str("4295128739").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(MIN_TICK + 1),
        U160::from_str("4295343490").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(MAX_TICK - 1),
        U160::from_str("1461373636630004318706518188784493106690254656249").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(MAX_TICK),
        U160::from_str("1461446703485210103287273052203988822378723970342").unwrap(),
    );
}

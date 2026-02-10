use alloy_primitives::{
    Address,
    aliases::{I24, I256, U160, U256},
    uint,
};
use num_bigint::BigUint;
use pyo3::{
    exceptions::{PyTypeError, PyValueError},
    prelude::*,
    types::{PyBytes, PyString},
};
use std::str::FromStr;

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

    let mut ratio: U256 = if abs_tick & U256::ONE != U256::ZERO {
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
        if (abs_tick & tick_mask) != U256::ZERO {
            ratio = (ratio * ratio_multiplier) >> 128;
        }
    }

    if tick > 0 {
        ratio = U256::MAX / ratio;
    }

    const ONE_SHL_32: U256 = uint!(0x100000000_U256);

    let mut sqrt_ratio: U256 = ratio >> 32;
    if (ratio % ONE_SHL_32) != U256::ZERO {
        sqrt_ratio += U256::ONE
    }

    sqrt_ratio.to::<U160>()
}

#[pyfunction]
pub fn get_tick_at_sqrt_ratio_alloy_translator(sqrt_price_x96: BigUint) -> PyResult<i32> {
    let mut sqrt_price_x96 = sqrt_price_x96.to_bytes_le();

    // pad the vec with zeros until it meets the expected length (20 bytes)
    if sqrt_price_x96.len() < 20 {
        sqrt_price_x96.resize(20, 0);
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

    const FACTOR_SHIFT_VALUES: [(u128, u8); 8] = [
        (0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF, 7),
        (0xFFFFFFFFFFFFFFFF, 6),
        (0xFFFFFFFF, 5),
        (0xFFFF, 4),
        (0xFF, 3),
        (0xF, 2),
        (0x3, 1),
        (0x1, 0),
    ];
    for (factor, shift_bits) in FACTOR_SHIFT_VALUES {
        f = U256::from(r > factor) << shift_bits;
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

    const LOG_SQRT10001_MULT: &str = "255738958999603826347141";
    const TICK_LOW_OFFSET: &str = "3402992956809132418596140100660247210";
    const TICK_HIGH_OFFSET: &str = "291339464771989622907027621153398088495";

    let log_sqrt10001: I256 = log_2 * I256::unchecked_from(LOG_SQRT10001_MULT);

    // TODO: investigate why .asr(128) works, but >> 128 fails
    let tick_low: I24 = I24::from((log_sqrt10001 - I256::unchecked_from(TICK_LOW_OFFSET)).asr(128));
    let tick_high: I24 =
        I24::from((log_sqrt10001 + I256::unchecked_from(TICK_HIGH_OFFSET)).asr(128));

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

#[pyfunction]
pub fn to_checksum_address(address: Bound<'_, PyAny>) -> PyResult<String> {
    if address.is_instance_of::<PyString>() {
        let addr = Address::from_str(address.extract()?)
            .map_err(|e| PyErr::new::<PyValueError, _>(format!("Invalid address: {}", e)))?;
        Ok(addr.to_checksum(None))
    } else if address.is_instance_of::<PyBytes>() {
        if address.len()? != 20 {
            return Err(PyErr::new::<PyValueError, _>("Address must be 20 bytes"));
        }
        let address = Address::from_slice(address.extract()?);
        Ok(address.to_checksum(None))
    } else {
        Err(PyErr::new::<PyTypeError, _>(
            "Address must be string or bytes",
        ))
    }
}

#[pymodule]
fn degenbot_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(
        get_sqrt_ratio_at_tick_alloy_translator,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        get_tick_at_sqrt_ratio_alloy_translator,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(to_checksum_address, m)?)?;
    Ok(())
}

#[test]
fn test_tickmath() {
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

#[test]
fn test_tickmath_mid_values() {
    use std::str::FromStr;

    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(0),
        U160::from_str("79228162514264337593543950336").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(1),
        U160::from_str("79232123823359799118286999568").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(-1),
        U160::from_str("79224201403219477170569942574").unwrap()
    );
}

#[test]
fn test_tickmath_negative_values() {
    use std::str::FromStr;

    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(-100000),
        U160::from_str("533968626430936354154228408").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(-500000),
        U160::from_str("1101692437043807371").unwrap()
    );
}

#[test]
fn test_tickmath_positive_values() {
    use std::str::FromStr;

    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(100000),
        U160::from_str("11755562826496067164730007768450").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(500000),
        U160::from_str("5697689776495288729098254600827762987878").unwrap()
    );
}

#[test]
fn test_tickmath_additional_values() {
    use std::str::FromStr;

    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(10000),
        U160::from_str("130621891405341611593710811006").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(-10000),
        U160::from_str("48055510970269007215549348797").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(1000),
        U160::from_str("83290069058676223003182343270").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(-1000),
        U160::from_str("75364347830767020784054125655").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(100),
        U160::from_str("79625275426524748796330556128").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(-100),
        U160::from_str("78833030112140176575862854579").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(10),
        U160::from_str("79267784519130042428790663799").unwrap()
    );
    assert_eq!(
        get_sqrt_ratio_at_tick_alloy(-10),
        U160::from_str("79188560314459151373725315960").unwrap()
    );
}

#[test]
fn test_tickmath_roundtrip() {
    let ticks = [
        -500000, -100000, -10000, -1000, -100, -10, -1, 0, 1, 10, 100, 1000, 10000, 100000, 500000,
    ];

    for tick in ticks {
        let ratio = get_sqrt_ratio_at_tick_alloy(tick);
        let tick_back = get_tick_at_sqrt_ratio_alloy(ratio);
        assert_eq!(tick_back.as_i32(), tick);
    }
}

#[test]
fn test_tickmath_boundary_roundtrip() {
    let min_ratio = get_sqrt_ratio_at_tick_alloy(-887272);
    assert_eq!(
        get_tick_at_sqrt_ratio_alloy(min_ratio),
        I24::unchecked_from(-887272)
    );

    let max_ratio = get_sqrt_ratio_at_tick_alloy(887272);
    let max_ratio_minus_one = U256::from(max_ratio) - U256::ONE;
    assert_eq!(
        get_tick_at_sqrt_ratio_alloy(U160::from(max_ratio_minus_one)),
        I24::unchecked_from(887271)
    );
}

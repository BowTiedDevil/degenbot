use criterion::{criterion_group, criterion_main, Criterion};
use degenbot_rs::tick_math::{get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal};
use std::hint::black_box;

fn tick_to_price_benchmark(c: &mut Criterion) {
    c.bench_function("get_sqrt_ratio_at_tick(0)", |b| {
        b.iter(|| get_sqrt_ratio_at_tick_internal(black_box(0)))
    });

    c.bench_function("get_sqrt_ratio_at_tick(MAX_TICK)", |b| {
        b.iter(|| get_sqrt_ratio_at_tick_internal(black_box(887272)))
    });

    c.bench_function("get_sqrt_ratio_at_tick(MIN_TICK)", |b| {
        b.iter(|| get_sqrt_ratio_at_tick_internal(black_box(-887272)))
    });

    c.bench_function("get_sqrt_ratio_at_tick(mixed)", |b| {
        b.iter(|| {
            for tick in [-100000, -1000, 0, 1000, 100000].iter() {
                let result = get_sqrt_ratio_at_tick_internal(*tick);
                let _ = black_box(result);
            }
        })
    });
}

fn price_to_tick_benchmark(c: &mut Criterion) {
    use alloy_primitives::U160;
    use std::str::FromStr;

    let ratio_0 = U160::from_str("79228162514264337593543950336").unwrap();
    let ratio_max = U160::from_str("1461446703485210103287273052203988822378723970342").unwrap();
    let ratio_min = U160::from_str("4295128739").unwrap();

    c.bench_function("get_tick_at_sqrt_ratio(mid)", |b| {
        b.iter(|| get_tick_at_sqrt_ratio_internal(black_box(ratio_0)))
    });

    c.bench_function("get_tick_at_sqrt_ratio(max)", |b| {
        b.iter(|| get_tick_at_sqrt_ratio_internal(black_box(ratio_max)))
    });

    c.bench_function("get_tick_at_sqrt_ratio(min)", |b| {
        b.iter(|| get_tick_at_sqrt_ratio_internal(black_box(ratio_min)))
    });

    c.bench_function("get_tick_at_sqrt_ratio(mixed)", |b| {
        b.iter(|| {
            for ratio in [
                U160::from_str("533968626430936354154228408").unwrap(),
                U160::from_str("1101692437043807371").unwrap(),
                U160::from_str("79228162514264337593543950336").unwrap(),
                U160::from_str("11755562826496067164730007768450").unwrap(),
                U160::from_str("5697689776495288729098254600827762987878").unwrap(),
            ]
            .iter()
            {
                let result = get_tick_at_sqrt_ratio_internal(*ratio);
                let _ = black_box(result);
            }
        })
    });
}

fn roundtrip_benchmark(c: &mut Criterion) {
    c.bench_function("roundtrip_tick_price_tick", |b| {
        b.iter(|| {
            let tick = black_box(50000);
            let price = get_sqrt_ratio_at_tick_internal(tick);
            if let Ok(price) = price {
                let tick_back = get_tick_at_sqrt_ratio_internal(price);
                if let Ok(tick_back) = tick_back {
                    assert_eq!(tick_back.as_i32(), tick);
                }
            }
        })
    });
}

criterion_group!(
    benches,
    tick_to_price_benchmark,
    price_to_tick_benchmark,
    roundtrip_benchmark
);
criterion_main!(benches);

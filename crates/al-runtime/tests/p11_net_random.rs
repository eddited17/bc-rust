//! BC's Randomize(Seed) creates `new System.Random(seed)`; Random(n) is `Next(n) + 1`
//! (Ncl ALSystemNumeric). The seeded sequence must match .NET's so that LibraryRandom data in
//! Microsoft's tests (seeded with 1 by their runner) are the values Microsoft's CI sees.
use al_runtime::builtins::NetRandom;

#[test]
fn seed_1_matches_dotnet() {
    // new System.Random(1).Next() on .NET: 534011718, 237820880, 1002897798, 1657007234, 1412011072
    let mut r = NetRandom::new(1);
    let got: Vec<i32> = (0..5).map(|_| r.sample_int()).collect();
    assert_eq!(got, vec![534011718, 237820880, 1002897798, 1657007234, 1412011072]);
}

#[test]
fn next_below_scales_the_sample() {
    // new System.Random(1).Next(100) on .NET: 24, 11, 46, 77, 65
    let mut r = NetRandom::new(1);
    let got: Vec<i32> = (0..5).map(|_| r.next_below(100)).collect();
    assert_eq!(got, vec![24, 11, 46, 77, 65]);
}

// Trivial Rust waterbox guest: proves rustc's std (Vec, sort, hashing) can be
// compiled for the musl guest with the large code model and linked into a
// core.wbx via musl-gcc + emulibc + linkscript. No ruffle yet - this only
// de-risks the toolchain. panic=abort (no unwinder), no stdio (no rt init).
#![no_main]

use std::vec::Vec;

// A deterministic computation touching the heap and std: build a vector, sort
// it descending, fold it into a digest. Native and sandbox must agree.
fn compute() -> u64 {
    let mut v: Vec<u64> = (0..1000u64).map(|i| (i.wrapping_mul(2654435761)) & 0xffff).collect();
    v.sort_unstable();
    v.reverse();
    let mut h: u64 = 1469598103934665603;
    for x in &v {
        h ^= *x;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

static mut STATE: u64 = 0;

#[no_mangle]
pub extern "C" fn Init() -> i32 {
    unsafe { STATE = compute(); }
    1
}

#[no_mangle]
pub extern "C" fn FrameAdvance() {
    unsafe { STATE = STATE.wrapping_add(compute()); }
}

#[no_mangle]
pub extern "C" fn GetDigest() -> u64 {
    unsafe { STATE }
}

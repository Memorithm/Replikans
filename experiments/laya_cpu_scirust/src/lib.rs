//! Private, single-process benchmark bridge; not a production execution API.
//! Caller owns initialized, disjoint contiguous f32 buffers for the entire call.
use scirust_simd::gemm::sgemm_parallel;
use scirust_simd::matrix::gemm_plan::GemmPlanF32;
use scirust_simd::matrix::workspace_gemm::GemmWorkspaceF32;
use std::cell::RefCell;
use std::collections::HashMap;

type Plans = HashMap<(usize, usize, usize), (GemmPlanF32, GemmWorkspaceF32)>;
thread_local! { static PLANS: RefCell<Plans> = RefCell::new(HashMap::new()); }

/// Return one only when the AVX-512 path used by this experiment is executable.
#[no_mangle]
pub extern "C" fn laya_probe_avx512() -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        u32::from(std::is_x86_feature_detected!("avx512f"))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

/// Compute C=A*B through the actual SciRust crate; no model or policy fallback.
///
/// # Safety
/// Pointers must be aligned, nonnull, and valid for the declared element counts.
/// A/B are read-only; C must not alias either, and must be initialized. The caller
/// must prevent concurrent mutation. This is an internal trusted benchmark ABI.
#[no_mangle]
pub unsafe extern "C" fn laya_probe_gemm(
    a: *const f32,
    b: *const f32,
    c: *mut f32,
    m: usize,
    k: usize,
    n: usize,
    mode: u32,
    threads: usize,
) -> i32 {
    if a.is_null() || b.is_null() || c.is_null() || mode > 1 || !(1..=8).contains(&threads) {
        return 1;
    }
    let extents = [m.checked_mul(k), k.checked_mul(n), m.checked_mul(n)];
    if extents
        .iter()
        .any(|v| v.is_none_or(|v| v == 0 || v > 32_000_000))
    {
        return 2;
    }
    // SAFETY: buffer ownership and extent validity are the caller's ABI contract;
    // overflow/zero/experiment size bounds have been checked above.
    let a = unsafe { std::slice::from_raw_parts(a, m * k) };
    let b = unsafe { std::slice::from_raw_parts(b, k * n) };
    let c = unsafe { std::slice::from_raw_parts_mut(c, m * n) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if mode == 1 {
            sgemm_parallel(1.0, a, m, k, b, n, 0.0, c, threads);
            Ok(())
        } else {
            PLANS.with(|plans| {
                let mut plans = plans.borrow_mut();
                let count = plans.len();
                let (plan, workspace) = match plans.entry((m, k, n)) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        if count >= 64 {
                            return Err(());
                        }
                        let plan = GemmPlanF32::prepare(m, k, n).map_err(|_| ())?;
                        let workspace = plan.create_workspace();
                        entry.insert((plan, workspace))
                    }
                };
                plan.execute(1.0, a, b, 0.0, c, workspace).map_err(|_| ())
            })
        }
    }));
    match result {
        Ok(Ok(())) => 0,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_matches_independent_f64_oracle_in_both_modes() {
        // Non-tile-aligned dimensions exercise masked rows, columns and K tails.
        let (m, k, n) = (17, 23, 19);
        let a: Vec<f32> = (0..m * k).map(|i| (i % 29) as f32 * 0.025 - 0.3).collect();
        let b: Vec<f32> = (0..k * n).map(|i| (i % 31) as f32 * 0.02 - 0.25).collect();
        for mode in [0, 1] {
            for _ in 0..2 {
                // Repeated execution also covers cached workspace reuse.
                let mut c = vec![123.0_f32; m * n];
                // SAFETY: live disjoint owned buffers with the exact declared lengths.
                let status = unsafe {
                    laya_probe_gemm(a.as_ptr(), b.as_ptr(), c.as_mut_ptr(), m, k, n, mode, 2)
                };
                assert_eq!(status, 0);
                for row in 0..m {
                    for col in 0..n {
                        let expected: f64 = (0..k)
                            .map(|p| f64::from(a[row * k + p]) * f64::from(b[p * n + col]))
                            .sum();
                        assert!((f64::from(c[row * n + col]) - expected).abs() < 1e-5);
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_shapes_and_modes_do_not_touch_output() {
        let a = [1.0_f32];
        let b = [2.0_f32];
        let mut c = [7.0_f32];
        for (m, k, n, mode, threads) in [
            (usize::MAX, 2, 1, 0, 1),
            (0, 1, 1, 0, 1),
            (1, 1, 1, 2, 1),
            (1, 1, 1, 0, 9),
        ] {
            // SAFETY: invalid parameters are rejected before any buffer is read.
            let status = unsafe {
                laya_probe_gemm(
                    a.as_ptr(),
                    b.as_ptr(),
                    c.as_mut_ptr(),
                    m,
                    k,
                    n,
                    mode,
                    threads,
                )
            };
            assert_ne!(status, 0);
            assert_eq!(c, [7.0]);
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright The Lance Authors
//
// Vendored verbatim from lance-format/lance @ 5cf70b27b3ad38ecdcd1547b7af385e05f67598a
// Original path: rust/lance-core/src/utils/assume.rs
//
// `assume!` / `assume_eq!` are the load-bearing macros that let LLVM elide
// bounds checks and auto-vectorize in release. `debug_assert_eq!` alone
// gets stripped in release; this macro additionally informs the optimizer
// via `std::hint::assert_unchecked`.

/// A macro that combines debug_assert and std::hint::assert_unchecked for optimized assertions
///
/// In debug builds, this will perform a normal assertion check.
/// In release builds, this will use hint::assert_unchecked which tells the compiler to assume
/// the condition is true without actually checking it.
///
/// # Safety
///
/// This macro is unsafe in release builds since it uses hint::assert_unchecked.
/// The caller must ensure the condition will always be true.
#[macro_export]
macro_rules! assume {
    ($cond:expr) => {
        debug_assert!($cond);
        // SAFETY: The debug_assert ensures this is true in debug builds.
        // In release builds, caller must ensure the condition holds.
        unsafe { std::hint::assert_unchecked($cond); }
    };
    ($cond:expr, $($arg:tt)+) => {
        debug_assert!($cond, $($arg)+);
        // SAFETY: The debug_assert ensures this is true in debug builds.
        // In release builds, caller must ensure the condition holds.
        unsafe { std::hint::assert_unchecked($cond); }
    };
}

/// Helper macro for equality assumptions.
#[macro_export]
macro_rules! assume_eq {
    ($left:expr, $right:expr) => {
        debug_assert_eq!($left, $right);
        unsafe { std::hint::assert_unchecked($left == $right); }
    };
    ($left:expr, $right:expr, $($arg:tt)+) => {
        debug_assert_eq!($left, $right, $($arg)+);
        unsafe { std::hint::assert_unchecked($left == $right); }
    };
}

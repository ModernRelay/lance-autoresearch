//! Default tolerance constants for bit-exact correctness oracles.
//!
//! These suit float-arithmetic kernels (PQ distance, BM25 scoring, vector
//! normalization) where SIMD-accumulator reordering is legal but real bugs
//! shift values by orders of magnitude. Targets that operate on integer or
//! byte-exact data (bitpack decode, dictionary decode, FSST decode) should
//! assert strict bitwise equality and not use these constants.

/// Maximum permitted absolute element error between agent kernel output and
/// reference output, for float kernels. Applied to both the distance table
/// and the per-vector distances vec.
pub const MAX_ABS_ERR: f32 = 1e-4;

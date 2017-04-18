
extern crate alga;
extern crate nalgebra as na;
extern crate num;
#[macro_use] extern crate ndarray;
extern crate ndarray_parallel;
extern crate generic_array;
#[macro_use] extern crate mopa;
extern crate rayon;
extern crate typenum;
extern crate cgmath;

extern crate sprs;

#[macro_use] pub mod macros;

pub mod cg;
pub mod dec;
pub mod grid;
pub mod math;
pub mod particle;
pub mod pcg;
pub mod solver;
pub mod sparse;
pub mod sph;
pub mod wavelet;

pub use grid::*;
pub use solver::*;

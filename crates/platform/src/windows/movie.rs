//! Native Media Foundation conversion, encoding and verified-file decoding.

mod convert;
pub(crate) mod encoder;
mod file;
mod foundation;
pub(super) mod read;

#[cfg(test)]
mod tests;

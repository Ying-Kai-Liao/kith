//! M3 lane: conversations. The claim code (M3-2) is `claims`: the start protocol of runtime-design section 4 over the
//! tables of migration 80. The wire paths (holder pipes, the warm runner, `terminal.open`) call it later.

pub(crate) mod basis;
#[cfg(test)]
mod basis_tests;
pub(crate) mod claims;
#[cfg(test)]
mod props;
#[cfg(test)]
mod tests;
pub(crate) mod warm;

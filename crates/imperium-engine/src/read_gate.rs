//! A seam for tests of parallel reads (`harness.parallel_reads`): a future every read of a
//! run waits for before it starts reading, so a test can hold reads open (a barrier only
//! overlapping reads pass, or a read that never finishes) without a slow file system. Nothing
//! sets it outside tests.

use std::future::Future;
use std::pin::Pin;

use imperium_tools::Invocation;

pub trait ReadGate: Send + Sync + std::fmt::Debug {
    /// Resolves when `invocation`, one of the reads a step runs ahead together, may read.
    fn before<'a>(
        &'a self,
        invocation: &'a Invocation,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

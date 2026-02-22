#![no_std]

/// Placeholder agent trait. All agents implement this.
pub trait Agent {
    /// Process an input message and return a response.
    fn run(&self, input: &str) -> &str;
}

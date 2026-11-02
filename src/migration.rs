/// Converts a module's persisted or external input before it is published.
/// The caller owns startup ordering, persistence and atomic application.
pub trait Migration<Input> {
    type Output;
    type Error;

    fn migrate(&self, input: Input) -> Result<Self::Output, Self::Error>;
}

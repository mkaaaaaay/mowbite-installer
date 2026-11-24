use std::fmt;

/// A value that must never reach a log, a page or an error text: no Display, Debug prints a mask.
pub struct Secret<T>(T);

/// `bytes` random bytes from the system in hex.
pub fn random_hex(bytes: usize) -> anyhow::Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf)
        .map_err(|e| anyhow::anyhow!("no randomness from the system: {e}"))?;
    Ok(crate::store::hex(&buf))
}

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Secret(value)
    }

    /// Only where the value goes into the device image itself.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_value() {
        let s = Secret::new("hunter2".to_string());
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(s.expose(), "hunter2");
    }
}

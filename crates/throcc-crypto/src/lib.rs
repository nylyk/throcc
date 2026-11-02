#[cfg(feature = "media")]
pub mod hpke;
pub mod identity;
#[cfg(feature = "media")]
pub mod media;
pub mod pake;
pub mod safety;

#[cfg(test)]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

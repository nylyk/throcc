//! Fake access units: an index, then bytes derived from it, so a receiver can check each one.
//! The integration tests include this file by path, so it uses nothing else from this crate.

use bytes::Bytes;

/// Fake access unit `index`, at least 4 bytes long.
pub fn unit(index: u32, len: usize) -> Bytes {
    let mut data = vec![0u8; len.max(4)];
    data[..4].copy_from_slice(&index.to_be_bytes());
    for (i, b) in data.iter_mut().enumerate().skip(4) {
        *b = filler(index, i);
    }
    Bytes::from(data)
}

/// The index `data` claims to be.
pub fn unit_index(data: &[u8]) -> Option<u32> {
    data.first_chunk().map(|b| u32::from_be_bytes(*b))
}

/// Whether `data` is exactly the unit its index names.
pub fn is_unit(data: &[u8]) -> bool {
    unit_index(data).is_some_and(|index| {
        data.iter()
            .enumerate()
            .skip(4)
            .all(|(i, b)| *b == filler(index, i))
    })
}

fn filler(index: u32, i: usize) -> u8 {
    (index as usize).wrapping_mul(31).wrapping_add(i) as u8
}

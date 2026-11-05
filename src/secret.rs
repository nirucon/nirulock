//! The password entry buffer.
//!
//! The entry is a [`Zeroizing`] string whose capacity is reserved up front,
//! so it never reallocates and leaves no stale copy in freed memory. Erasing
//! a character also overwrites the bytes it occupied.

use zeroize::{Zeroize, Zeroizing};

/// The longest entry accepted, in bytes.
pub const MAX: usize = 4096;

pub fn new_entry() -> Zeroizing<String> {
    Zeroizing::new(String::with_capacity(MAX))
}

/// Append `text` if it fits; the entry never grows past its capacity.
pub fn push(entry: &mut Zeroizing<String>, text: &str) {
    if entry.len() + text.len() <= MAX.min(entry.capacity()) {
        entry.push_str(text);
    }
}

/// Remove the last character and overwrite its bytes.
pub fn erase_last(entry: &mut Zeroizing<String>) {
    if let Some(c) = entry.pop() {
        // SAFETY: only bytes beyond the string's length are written, so the
        // string stays valid UTF-8.
        let bytes = unsafe { entry.as_mut_vec() };
        let spare = bytes.spare_capacity_mut();
        let n = c.len_utf8().min(spare.len());
        spare[..n].zeroize();
    }
}

/// Empty the entry and overwrite everything it held.
pub fn clear(entry: &mut Zeroizing<String>) {
    // Zeroize clears the string and wipes its whole capacity.
    entry.zeroize();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(entry: &Zeroizing<String>, n: usize) -> Vec<u8> {
        // SAFETY: reads within the allocation; the bytes were written by
        // push_str and then by the wiping under test.
        unsafe { std::slice::from_raw_parts(entry.as_ptr(), n).to_vec() }
    }

    #[test]
    fn erasing_overwrites_the_removed_bytes() {
        let mut entry = new_entry();
        push(&mut entry, "aé");
        let ptr = entry.as_ptr();
        erase_last(&mut entry);
        assert_eq!(entry.as_str(), "a");
        assert_eq!(raw(&entry, 3), [b'a', 0, 0]);
        assert_eq!(entry.as_ptr(), ptr, "the entry reallocated");
    }

    #[test]
    fn clearing_overwrites_everything() {
        let mut entry = new_entry();
        push(&mut entry, "fixture");
        clear(&mut entry);
        assert!(entry.is_empty());
        assert!(entry.capacity() >= 7);
        assert!(raw(&entry, 7).iter().all(|b| *b == 0));
    }

    #[test]
    fn the_entry_never_reallocates() {
        let mut entry = new_entry();
        let ptr = entry.as_ptr();
        for _ in 0..MAX + 10 {
            push(&mut entry, "x");
        }
        assert_eq!(entry.len(), MAX);
        assert_eq!(entry.as_ptr(), ptr);
    }
}

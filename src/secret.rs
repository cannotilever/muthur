//! The operator's sudo password. Zeroized on drop, never printed.

use zeroize::{Zeroize, Zeroizing};

const INITIAL_CAP: usize = 128;

pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new() -> Self {
        Secret(Zeroizing::new(String::with_capacity(INITIAL_CAP)))
    }

    pub fn push(&mut self, c: char) {
        if self.0.len() + c.len_utf8() > self.0.capacity() {
            // Grow by hand so the old buffer is zeroized, not freed with copies in it.
            let mut bigger = Zeroizing::new(String::with_capacity(self.0.capacity() * 2));
            bigger.push_str(&self.0);
            self.0 = bigger; // old Zeroizing drops and wipes
        }
        self.0.push(c);
    }

    pub fn pop(&mut self) {
        if let Some(c) = self.0.chars().next_back() {
            let start = self.0.len() - c.len_utf8();
            // SAFETY: NUL bytes are valid UTF-8, and the string is truncated right after.
            unsafe { self.0.as_bytes_mut()[start..].zeroize() };
            self.0.truncate(start);
        }
    }

    pub fn chars(&self) -> usize {
        self.0.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.zeroize();
    }

    /// A separate zeroizing copy, so two owners can each wipe their own.
    pub fn duplicate(&self) -> Secret {
        let mut d = Secret(Zeroizing::new(String::with_capacity(
            self.0.capacity().max(INITIAL_CAP),
        )));
        d.0.push_str(&self.0);
        d
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Default for Secret {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let mut s = Secret::new();
        for c in "hunter2".chars() {
            s.push(c);
        }
        assert_eq!(format!("{:?}", s), "Secret(..)");
    }

    #[test]
    fn push_pop_count() {
        let mut s = Secret::new();
        for c in "hux".chars() {
            s.push(c);
        }
        s.pop();
        assert_eq!(s.chars(), 2);
        assert_eq!(s.expose(), "hu");
    }

    #[test]
    fn clear_empties() {
        let mut s = Secret::new();
        s.push('a');
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.expose(), "");
    }

    #[test]
    fn pop_wipes_removed_bytes() {
        let mut s = Secret::new();
        s.push('a');
        s.push('é');
        s.pop();
        assert_eq!(s.expose(), "a");
        let v = unsafe { s.0.as_mut_vec() };
        let spare = v.spare_capacity_mut();
        assert!(spare[..2].iter().all(|b| unsafe { b.assume_init() } == 0));
    }

    #[test]
    fn grows_past_initial_capacity() {
        let mut s = Secret::new();
        for _ in 0..300 {
            s.push('x');
        }
        assert_eq!(s.chars(), 300);
    }

    #[test]
    fn duplicate_is_independent() {
        let mut s = Secret::new();
        for c in "abc".chars() {
            s.push(c);
        }
        let d = s.duplicate();
        s.clear();
        assert_eq!(d.expose(), "abc");
        assert!(s.is_empty());
    }

    #[test]
    fn is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Secret>();
    }
}

//! Process-local string interner. Hot structures store `Symbol`s; original
//! strings are kept for emission and diagnostics (see port plan §Interner).

use rustc_hash::FxHashMap;

/// An interned string handle. Cheap to copy and compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(pub u32);

#[derive(Debug, Default)]
pub struct Interner {
    strings: Vec<Box<str>>,
    map: FxHashMap<Box<str>, Symbol>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, s: &str) -> Symbol {
        if let Some(&sym) = self.map.get(s) {
            return sym;
        }
        let sym = Symbol(self.strings.len() as u32);
        let boxed: Box<str> = s.into();
        self.strings.push(boxed.clone());
        self.map.insert(boxed, sym);
        sym
    }

    pub fn resolve(&self, sym: Symbol) -> &str {
        &self.strings[sym.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_and_dedups() {
        let mut i = Interner::new();
        let a = i.intern("Patient");
        let b = i.intern("Patient");
        let c = i.intern("Observation");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(i.resolve(a), "Patient");
        assert_eq!(i.len(), 2);
    }
}

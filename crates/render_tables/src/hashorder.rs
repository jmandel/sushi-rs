//! Java `HashMap<String,String>` iteration-order emulation.
//!
//! fhir-core's `XhtmlNode.attributes` is a `HashMap<String,String>`
//! (XhtmlNode.java:142) and `XhtmlComposer.attributes` iterates
//! `getAttributes().keySet()` (XhtmlComposer.java:308). So the bytes the
//! publisher emits carry attributes in Java HashMap iteration order, NOT the
//! order the renderer set them in. Our `render_xhtml` OrderMap preserves
//! insertion order (correct for the C3 round-trip substrate — node.rs docs), so
//! when WE synthesize table fragments we must insert attributes in the order a
//! Java HashMap would have iterated them. This module computes that order.
//!
//! Java HashMap iteration walks `table[0..capacity]` and, within a bucket, the
//! chained entries in insertion order. `capacity` starts at 16 and doubles
//! whenever `size > capacity * 0.75`. Bucket index for a key is
//! `(capacity-1) & spread(key.hashCode())` where
//! `spread(h) = h ^ (h >>> 16)` (HashMap.hash). String.hashCode is the classic
//! `s[0]*31^(n-1) + ...` accumulation. Verified against the golden corpus:
//! e.g. the tbl_spacer img's set order src,style,class,alt emits as
//! src,alt,style,class — exactly this model.
//!
//! Treeification (bucket > 8 entries) never occurs for our attribute counts, so
//! within-bucket order is plain insertion order.

/// Java `String.hashCode()` (32-bit, wrapping).
pub fn java_string_hash(s: &str) -> i32 {
    let mut h: i32 = 0;
    // Java iterates UTF-16 code units; for the ASCII attribute names we deal
    // with, char == code unit. We iterate UTF-16 units to be exact.
    for u in s.encode_utf16() {
        h = h.wrapping_mul(31).wrapping_add(u as i32);
    }
    h
}

/// Java `HashMap.hash(key)` spread: `h ^ (h >>> 16)`.
fn spread(h: i32) -> u32 {
    let hu = h as u32;
    hu ^ (hu >> 16)
}

/// Return `keys` reordered into Java `HashMap` iteration order, given they were
/// `put` in the slice's order (insertion order). Stable within a bucket.
pub fn hashmap_order<T: AsRef<str> + Clone>(keys: &[T]) -> Vec<T> {
    let n = keys.len();
    let mut cap: usize = 16;
    // Java: resize when ++size > threshold (threshold = cap*0.75). After all
    // puts, capacity is the smallest power-of-two >= 16 with n <= cap*0.75.
    while (n as f64) > (cap as f64) * 0.75 {
        cap <<= 1;
    }
    let mask = (cap - 1) as u32;
    // Stable sort by bucket index; ties keep original (insertion) order.
    let mut indexed: Vec<(usize, T)> = keys.iter().cloned().enumerate().collect();
    indexed.sort_by_key(|(i, k)| {
        let bucket = mask & spread(java_string_hash(k.as_ref()));
        (bucket, *i as u32)
    });
    indexed.into_iter().map(|(_, k)| k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn img_spacer_order() {
        // Set order in HTG:1078: src, style, class, alt. Golden emits
        // src, alt, style, class.
        let got = hashmap_order(&["src", "style", "class", "alt"]);
        assert_eq!(got, vec!["src", "alt", "style", "class"]);
    }

    #[test]
    fn table_order() {
        // set: border, cellspacing, cellpadding, style -> golden: border,
        // cellpadding, cellspacing, style
        let got = hashmap_order(&["border", "cellspacing", "cellpadding", "style"]);
        assert_eq!(got, vec!["border", "cellpadding", "cellspacing", "style"]);
    }

    #[test]
    fn th_cell_order() {
        let got = hashmap_order(&["style", "class"]);
        assert_eq!(got, vec!["style", "class"]);
    }
}

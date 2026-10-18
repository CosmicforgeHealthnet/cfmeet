//! Append-only key-transparency log: the RFC 9162 §2.1 Merkle Tree over SHA-256.
//!
//! * `leaf_hash(d) = SHA-256(0x00 || d)`, `node_hash(l, r) = SHA-256(0x01 || l || r)`,
//!   the empty tree hashes to `SHA-256("")`.
//! * Storage model: only PERFECT, aligned subtrees are stored — `(level, index)` is the
//!   root of leaves `[index·2^level, (index+1)·2^level)`. Appending leaf `i` adds the leaf
//!   and every perfect ancestor it completes ([`append`]), O(log n) nodes. Every subtree
//!   an RFC 9162 algorithm needs is aligned, so tree heads and proofs need only
//!   O(log² n) stored nodes ([`required_for_root`], [`required_for_inclusion`],
//!   [`required_for_consistency`]).
//! * [`verify_inclusion`] and [`verify_consistency`] are the RFC 9162 §2.1.3.2 /
//!   §2.1.4.2 verification algorithms, for clients and auditors.
//!
//! This is a verifiable append-only structure. On its own it does NOT prevent a
//! malicious log from showing different views to different clients (equivocation): that
//! needs independent witnesses / gossip, an open architecture decision (ADR 0045).

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];

/// Coordinates of a stored perfect subtree.
pub type NodeId = (u32, u64);

pub fn leaf_hash(data: &[u8]) -> Hash {
    let mut h = Sha256::new();
    h.update([0x00]);
    h.update(data);
    h.finalize().into()
}

pub fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update([0x01]);
    h.update(left);
    h.update(right);
    h.finalize().into()
}

pub fn empty_root() -> Hash {
    Sha256::digest([]).into()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProofError {
    #[error("a stored node is missing")]
    MissingNode(NodeId),
    #[error("the requested index or sizes are out of range")]
    OutOfRange,
}

/// Largest power of two strictly smaller than `n` (`n ≥ 2`).
fn split(n: u64) -> u64 {
    debug_assert!(n >= 2);
    1u64 << (63 - (n - 1).leading_zeros())
}

/// MTH of leaves `[start, start + size)`; `start` is aligned to the largest power of two
/// not above `size` (true for every range the algorithms request).
fn subtree<F>(start: u64, size: u64, node: &mut F) -> Result<Hash, ProofError>
where
    F: FnMut(NodeId) -> Result<Hash, ProofError>,
{
    if size.is_power_of_two() {
        let level = size.trailing_zeros();
        return node((level, start >> level));
    }
    let k = split(size);
    let left = subtree(start, k, node)?;
    let right = subtree(start + k, size - k, node)?;
    Ok(node_hash(&left, &right))
}

fn root_with<F>(size: u64, node: &mut F) -> Result<Hash, ProofError>
where
    F: FnMut(NodeId) -> Result<Hash, ProofError>,
{
    if size == 0 {
        Ok(empty_root())
    } else {
        subtree(0, size, node)
    }
}

fn path_with<F>(
    index: u64,
    start: u64,
    size: u64,
    node: &mut F,
    out: &mut Vec<Hash>,
) -> Result<(), ProofError>
where
    F: FnMut(NodeId) -> Result<Hash, ProofError>,
{
    if size <= 1 {
        return Ok(());
    }
    let k = split(size);
    if index < k {
        path_with(index, start, k, node, out)?;
        out.push(subtree(start + k, size - k, node)?);
    } else {
        path_with(index - k, start + k, size - k, node, out)?;
        out.push(subtree(start, k, node)?);
    }
    Ok(())
}

fn subproof_with<F>(
    m: u64,
    start: u64,
    size: u64,
    complete: bool,
    node: &mut F,
    out: &mut Vec<Hash>,
) -> Result<(), ProofError>
where
    F: FnMut(NodeId) -> Result<Hash, ProofError>,
{
    if m == size {
        if !complete {
            out.push(subtree(start, size, node)?);
        }
        return Ok(());
    }
    let k = split(size);
    if m <= k {
        subproof_with(m, start, k, complete, node, out)?;
        out.push(subtree(start + k, size - k, node)?);
    } else {
        subproof_with(m - k, start + k, size - k, false, node, out)?;
        out.push(subtree(start, k, node)?);
    }
    Ok(())
}

/// The tree head of a tree of `size` leaves.
pub fn root<F>(size: u64, mut node: F) -> Result<Hash, ProofError>
where
    F: FnMut(NodeId) -> Result<Hash, ProofError>,
{
    root_with(size, &mut node)
}

/// RFC 9162 `PATH(index, D[size])`: the inclusion proof of leaf `index`.
pub fn inclusion_proof<F>(index: u64, size: u64, mut node: F) -> Result<Vec<Hash>, ProofError>
where
    F: FnMut(NodeId) -> Result<Hash, ProofError>,
{
    if index >= size {
        return Err(ProofError::OutOfRange);
    }
    let mut out = Vec::new();
    path_with(index, 0, size, &mut node, &mut out)?;
    Ok(out)
}

/// RFC 9162 `PROOF(old, D[new])`: the consistency proof between two tree sizes.
pub fn consistency_proof<F>(old: u64, new: u64, mut node: F) -> Result<Vec<Hash>, ProofError>
where
    F: FnMut(NodeId) -> Result<Hash, ProofError>,
{
    if old > new {
        return Err(ProofError::OutOfRange);
    }
    if old == 0 || old == new {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    subproof_with(old, 0, new, true, &mut node, &mut out)?;
    Ok(out)
}

fn collect(
    run: impl FnOnce(&mut dyn FnMut(NodeId) -> Result<Hash, ProofError>),
) -> BTreeSet<NodeId> {
    let mut needed = BTreeSet::new();
    let mut record = |id: NodeId| {
        needed.insert(id);
        Ok([0u8; 32])
    };
    run(&mut record);
    needed
}

/// Stored nodes [`root`] reads for `size`.
pub fn required_for_root(size: u64) -> BTreeSet<NodeId> {
    collect(|n| {
        let _ = root_with(size, &mut |id| n(id));
    })
}

/// Stored nodes [`inclusion_proof`] reads.
pub fn required_for_inclusion(index: u64, size: u64) -> BTreeSet<NodeId> {
    collect(|n| {
        let _ = inclusion_proof(index, size, n);
    })
}

/// Stored nodes [`consistency_proof`] reads.
pub fn required_for_consistency(old: u64, new: u64) -> BTreeSet<NodeId> {
    collect(|n| {
        let _ = consistency_proof(old, new, n);
    })
}

/// Left siblings [`append`] reads to append leaf `index`.
pub fn required_for_append(index: u64) -> BTreeSet<NodeId> {
    let mut needed = BTreeSet::new();
    let (mut level, mut i) = (0u32, index);
    while i & 1 == 1 {
        needed.insert((level, i - 1));
        level += 1;
        i >>= 1;
    }
    needed
}

/// Nodes to store when leaf `index` (the next free index) is appended: the leaf and each
/// perfect ancestor it completes.
pub fn append<F>(index: u64, leaf: Hash, mut node: F) -> Result<Vec<(NodeId, Hash)>, ProofError>
where
    F: FnMut(NodeId) -> Result<Hash, ProofError>,
{
    let mut out = vec![((0, index), leaf)];
    let (mut level, mut i, mut current) = (0u32, index, leaf);
    while i & 1 == 1 {
        let left = node((level, i - 1))?;
        current = node_hash(&left, &current);
        level += 1;
        i >>= 1;
        out.push(((level, i), current));
    }
    Ok(out)
}

/// RFC 9162 §2.1.3.2: verifies that `leaf` is leaf `index` of the tree `(size, root)`.
pub fn verify_inclusion(index: u64, size: u64, leaf: &Hash, proof: &[Hash], root: &Hash) -> bool {
    if index >= size {
        return false;
    }
    let (mut fn_, mut sn) = (index, size - 1);
    let mut r = *leaf;
    for p in proof {
        if sn == 0 {
            return false;
        }
        if fn_ & 1 == 1 || fn_ == sn {
            r = node_hash(p, &r);
            if fn_ & 1 == 0 {
                while fn_ & 1 == 0 && fn_ != 0 {
                    fn_ >>= 1;
                    sn >>= 1;
                }
            }
        } else {
            r = node_hash(&r, p);
        }
        fn_ >>= 1;
        sn >>= 1;
    }
    sn == 0 && r == *root
}

/// RFC 9162 §2.1.4.2: verifies that the tree `(new, new_root)` extends `(old, old_root)`.
pub fn verify_consistency(
    old: u64,
    new: u64,
    old_root: &Hash,
    new_root: &Hash,
    proof: &[Hash],
) -> bool {
    if old > new {
        return false;
    }
    if old == new {
        return proof.is_empty() && old_root == new_root;
    }
    if old == 0 {
        return proof.is_empty();
    }
    let mut path: Vec<Hash> = proof.to_vec();
    if old.is_power_of_two() {
        path.insert(0, *old_root);
    }
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    let (mut fn_, mut sn) = (old - 1, new - 1);
    while fn_ & 1 == 1 {
        fn_ >>= 1;
        sn >>= 1;
    }
    let (mut fr, mut sr) = (*first, *first);
    for c in rest {
        if sn == 0 {
            return false;
        }
        if fn_ & 1 == 1 || fn_ == sn {
            fr = node_hash(c, &fr);
            sr = node_hash(c, &sr);
            if fn_ & 1 == 0 {
                while fn_ & 1 == 0 && fn_ != 0 {
                    fn_ >>= 1;
                    sn >>= 1;
                }
            }
        } else {
            sr = node_hash(&sr, c);
        }
        fn_ >>= 1;
        sn >>= 1;
    }
    sn == 0 && fr == *old_root && sr == *new_root
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Reference RFC 9162 MTH over the full leaf list (no stored nodes).
    fn mth(leaves: &[Hash]) -> Hash {
        match leaves.len() {
            0 => empty_root(),
            1 => leaves[0],
            n => {
                let k = split(n as u64) as usize;
                node_hash(&mth(&leaves[..k]), &mth(&leaves[k..]))
            }
        }
    }

    fn reference_path(m: usize, leaves: &[Hash]) -> Vec<Hash> {
        let n = leaves.len();
        if n <= 1 {
            return vec![];
        }
        let k = split(n as u64) as usize;
        if m < k {
            let mut p = reference_path(m, &leaves[..k]);
            p.push(mth(&leaves[k..]));
            p
        } else {
            let mut p = reference_path(m - k, &leaves[k..]);
            p.push(mth(&leaves[..k]));
            p
        }
    }

    struct Log {
        nodes: BTreeMap<NodeId, Hash>,
        leaves: Vec<Hash>,
    }

    impl Log {
        fn build(n: usize) -> Self {
            let mut log = Log {
                nodes: BTreeMap::new(),
                leaves: Vec::new(),
            };
            for i in 0..n {
                let leaf = leaf_hash(format!("entry-{i}").as_bytes());
                for id in required_for_append(i as u64) {
                    assert!(log.nodes.contains_key(&id));
                }
                for (id, h) in append(i as u64, leaf, |id| log.get(id)).unwrap() {
                    log.nodes.insert(id, h);
                }
                log.leaves.push(leaf);
            }
            log
        }

        fn get(&self, id: NodeId) -> Result<Hash, ProofError> {
            self.nodes
                .get(&id)
                .copied()
                .ok_or(ProofError::MissingNode(id))
        }
    }

    #[test]
    fn rfc_9162_empty_and_single_leaf_hashes() {
        // SHA-256("") — RFC 9162 §2.1.1 empty tree.
        assert_eq!(
            empty_root()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let log = Log::build(1);
        assert_eq!(root(1, |id| log.get(id)).unwrap(), log.leaves[0]);
    }

    #[test]
    fn stored_node_roots_and_paths_match_the_reference_algorithm() {
        let log = Log::build(70);
        for size in 0..=70u64 {
            let r = root(size, |id| log.get(id)).unwrap();
            assert_eq!(r, mth(&log.leaves[..size as usize]), "size {size}");
            for id in required_for_root(size) {
                assert!(log.nodes.contains_key(&id));
            }
            for index in 0..size {
                let p = inclusion_proof(index, size, |id| log.get(id)).unwrap();
                assert_eq!(
                    p,
                    reference_path(index as usize, &log.leaves[..size as usize])
                );
                assert!(verify_inclusion(
                    index,
                    size,
                    &log.leaves[index as usize],
                    &p,
                    &r
                ));
                // A wrong leaf, position or root never verifies (the root binds the tree; the size
                // itself is bound by consistency proofs between tree heads).
                assert!(!verify_inclusion(
                    index,
                    size,
                    &leaf_hash(b"forged"),
                    &p,
                    &r
                ));
                if size > 1 {
                    assert!(!verify_inclusion(
                        (index + 1) % size,
                        size,
                        &log.leaves[index as usize],
                        &p,
                        &r
                    ));
                }
                assert!(!verify_inclusion(
                    index,
                    size,
                    &log.leaves[index as usize],
                    &p,
                    &leaf_hash(b"other root")
                ));
            }
        }
    }

    #[test]
    fn consistency_proofs_verify_and_detect_rewritten_history() {
        let log = Log::build(40);
        let mut forged = Log::build(40);
        // A log that rewrote leaf 3 after the fact.
        forged.leaves[3] = leaf_hash(b"substituted key");
        let roots: Vec<Hash> = (0..=40)
            .map(|s| root(s, |id| log.get(id)).unwrap())
            .collect();
        let forged_roots: Vec<Hash> = (0..=40)
            .map(|s| mth(&forged.leaves[..s as usize]))
            .collect();
        for old in 0..=40u64 {
            for new in old..=40u64 {
                let p = consistency_proof(old, new, |id| log.get(id)).unwrap();
                assert!(
                    verify_consistency(old, new, &roots[old as usize], &roots[new as usize], &p),
                    "{old}->{new}"
                );
                if old > 3 && new > old {
                    assert!(!verify_consistency(
                        old,
                        new,
                        &roots[old as usize],
                        &forged_roots[new as usize],
                        &p
                    ));
                }
            }
        }
        assert!(consistency_proof(5, 4, |id| log.get(id)).is_err());
        assert!(inclusion_proof(4, 4, |id| log.get(id)).is_err());
    }

    #[test]
    fn missing_nodes_fail_closed() {
        let mut log = Log::build(9);
        log.nodes.remove(&(3, 0));
        assert_eq!(
            root(9, |id| log.get(id)),
            Err(ProofError::MissingNode((3, 0)))
        );
    }
}

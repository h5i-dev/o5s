# Similarity ordering and framing: one algorithm for every domain (from the human, 2026-11-15)

## Why
The current grouped-repack layout (Exp1 V2, `GitGrouper`) is an **ad-hoc, Git-specific heuristic**. We picked it while pre-registering Exp1. It isn't from a paper or a tool, and no alternative ordering was ever compared.
- It partitions blobs by file extension and orders by full path, then oldest first (rev-list first-seen position, not commit time).
- It concatenates objects into fixed 4 MiB frames, each compressed with zstd-19.

That's a domain guess: "similar names imply similar content". It contradicts `docs/VISION.md`, where the core is domain-agnostic. **A good ordering is driven by content similarity, so it works the same for source, weights, packages and image layers.** Names and sizes are at most tie-breaking hints that fronts may supply.

## What the data says
**Exp1 V2 is not weak.**
- Against Git's normal `git gc` it is 0.51–0.69×.
- Against `gc --aggressive` it is 0.85 / 0.93 / 1.34 / 1.16 (ripgrep / jq / redis / git), geomean 1.05.
- It only failed the self-imposed per-repo cap (redis 1.336 > 1.30).

**Where the loss likely is (a hypothesis to verify):** the fixed 4 MiB frame cut. The unbounded V3 has the same order and no cuts, and it gets redis 0.84 and git 0.71. A file with a long history spans many frames, and each frame re-pays almost a full copy.

**Path ordering** also separates moved/renamed files and same-named files in different directories. Git's name-hash puts those together. The size of this effect isn't measured.

## Prior work
- **Ouyang, Memon, Suel, Trendafilov: Cluster-based delta compression of a collection of files** (WISE 2002).
  - Optimal pairwise delta selection reduces to a maximum-weight branching. Text clustering prunes the candidate graph.
  - The same group's follow-up orders files with a TSP heuristic and compresses each against its predecessor and the best other predecessor.
  - Both are content-only.
  - https://research.engineering.nyu.edu/~suel/papers/cluster.pdf
- **Migratory Compression** (FAST 2014): relocate similar chunks together before compression, and restore order on read. Up to about 2× on similar data. https://www.researchgate.net/publication/262277159
- **MinHash reordering before zstd**: https://www.eecs.yorku.ca/~aan/research/paper/Cascon2024_Qinxin.pdf ; greedy MinHash plus 2-opt in sortof-suite, https://github.com/dannyboy76705/sortof-suite
  - For generic archives the gains are often about 1% over extension order. Treat big wins as unproven until measured.
- Git `pack-objects` (type / name-hash / size, window 250, depth 50) is the specialist baseline.

## Design: a generic core building block (replaces Phase 3 step (3) "Git-style windowed delta search")
1. **Features.** Content-defined sampling super-features or MinHash per object. For large objects, use per chunk. Reuse Exp1c's Odess-style extractor. Domain-agnostic.
2. **Order.**
   - Build approximate similarity neighbours (an LSH on super-features). Then greedy chaining, so that each object follows its most similar unplaced neighbour, refined with 2-opt or a maximum-weight-branching approximation.
   - Front hints (path, name-hash, size, time) only break ties. The algorithm must work with no hints at all.
3. **Frame.**
   - Random-access frames, but cut where the next object is dissimilar, not at a fixed size.
   - Allow a variable frame cap, bounded by the read-latency threshold (p99 ≤ 25 ms).
   - Where a cut is forced inside a similar run, start the next frame with its most similar earlier object as a **zstd reference prefix** (patch-from), so the cut doesn't re-pay a full copy. Keep the reference depth bounded, so reads decode at most k frames.
4. **Front API role change.** `Grouper` / `GroupKey` become *hints* to this core orderer. They no longer decide the layout. `GitGrouper` becomes a hint provider.

## Evaluation (pre-register in PLAN.md before measuring)
- **Corpus:** the Exp1 corpus (sizes vs the same `gc --aggressive` baseline and thresholds as §4.5). Also the SmolLM2 checkpoints and a small set of wheels/npm tarballs or OCI layers, to show the same algorithm helps outside Git.
- **Ablations (each reported):**
  - current V2
  - similarity order with fixed 4 MiB frames
  - path order with similarity-cut frames plus reference prefix
  - the full design
  - the full design with no front hints
- **Process:** develop on ripgrep and jq (short runs). Run redis and git.git once, in the batch benchmark phase, under the bench lock. Implementation comes first.

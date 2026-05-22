# PR-C architecture: R2007 LibreDWG conformance

**Goal.** Today R2007 is a "false pass" — `dwgread 0.13.3` exits 0 because we emit
the R2004 layout under an R2007 signature, but it also LOG_ERRORs
`Invalid file_header->header_size` and never reaches our objects. PR-C
makes R2007 emit the **actual** R2007 wire format so `dwgread` reads it
without errors.

## Why R2007 needs its own codec, not a flag on R2004

LibreDWG version-dispatches based on the legacy file-header signature.
R2007 takes a completely different path (`read_r2007_meta_data`,
`decode_r2007.c`) — not a flag on the R2004 path. The differences:

| Concern | R2004 | R2007 |
|---|---|---|
| File header at 0x80 | 0x78-byte XOR-masked plain header | RS-encoded blob (3×255 + 219 pad → 984 bytes) wrapping LZ77-compressed Dwg_R2007_Header |
| Header struct | `Dwg_R2004_Header` (~120 bytes) | `Dwg_R2007_Header` (288 bytes, 36 × int64) |
| Pages map / sections map | Section-info + page-map system pages | Pages map + sections map — different field layout, RS-encoded wrapper |
| System pages | 20-byte system-page envelope + LZ77 + Adler-32 | Same envelope + LZ77 + Adler-32, BUT additionally RS-encoded (block_count = ⌈(size_comp+7)/239⌉ blocks of 255 bytes interleaved) |
| Section indexing | by Dwg_Section_Type enum | by hashcode (`section->hashcode = (uint64_t)…`) with explicit UTF-16LE section name |

## Phases

1. **Reed-Solomon (255, 239) encoder** — `bits/reed_solomon.rs`
   - GF(2^8) with primitive polynomial X^8+X^6+X^5+X^4+1 (0x171). Tables ported verbatim from LibreDWG's `reedsolomon.c`.
   - `rs_encode_block(data: &[u8; 239]) -> [u8; 16]` — Horner's-method long division by the 17-coefficient rsgen polynomial.
   - `rs_interleave(blocks: &[[u8; 255]]) -> Vec<u8>` — column-major.
   - `rs_deinterleave(bytes: &[u8], block_count, data_size) -> Vec<u8>` — inverse, parallels `decode_rs` (data-only, ignores parity columns).
   - Pinned by known-vector tests: f256_residue[1] = 0x69, rsgen[0] = 0x6a, RS(`[0; 239]`) parity = 0..0, RS round-trip on random data.

2. **Dwg_R2007_Header struct + LZ77 wrapper**
   - 36-field struct, 288 bytes, little-endian. Two-pass write (zero placeholders, fill once offsets resolved).
   - LZ77-compress, then prepend 32-byte metadata block: `seqence_crc(8) || seqence_key(8) || compr_crc(8) || compr_len(4) || len2(4)`.
   - Pad to 717 bytes (3 × 239). RS-encode. Interleave. Pad to 984 bytes.

3. **R2007 system-page wrapper**
   - Existing `write_system_page` continues to produce the 20-byte envelope + payload + Adler-32. For R2007, that whole blob then goes through RS interleaving with `block_count = ⌈(size_comp+7)/239⌉` blocks of 255 bytes, final size aligned to 8.

4. **R2007 pages map + sections map**
   - Pages map: `(id: int32, size: int32, address: int64)` records, RS-wrapped system page.
   - Sections map: per-section `(data_size, max_size, encrypted, hashcode, name_length, unknown, encoded, num_pages)` block, then name (UTF-16LE, padded to even length), then per-page `(offset, size, …)` records. RS-wrapped system page.
   - Section dispatch by hashcode — values match LibreDWG's hash table.

5. **`assemble_r2007` + `parse_r2007`** — new top-level dispatch sibling to `assemble_r2004`. R2007 calls `assemble_r2007`; R2004/R2010/R2013/R2018 continue calling `assemble_r2004` until their own PRs land.

6. **Update golden hash for R2007** in `tests/dwg_goldens.rs` and run the LibreDWG oracle CI to verify the conformance step actually advances.

## Out of scope (deferred to PR-D / PR-E)

- R2010 / R2013 deltas — they're sister versions of R2007 with small header-vars additions; will reuse the same codec base in PR-D.
- R2018 encrypted handle pages — PR-E. Its base is R2018 = R2007 + magic-byte XOR on handle pages + new 2nd-header layout.

## Verification

- Self-round-trip: `parse_r2007(assemble_r2007(p)) == p` for all R2007 fixtures.
- Workspace tests: `cargo test -p aec_cad`.
- Clippy + fmt clean.
- LibreDWG oracle: `dwgread -O DXF` on the R2007 fixture exits 0 with **no** `Invalid file_header->header_size` LOG_ERROR.

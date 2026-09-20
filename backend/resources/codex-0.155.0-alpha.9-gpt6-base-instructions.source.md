# codex-0.155.0-alpha.9 GPT-6 base instructions source

Provenance note for `codex-0.155.0-alpha.9-gpt6-base-instructions.md`. The template file
itself is stored verbatim; this note only records where it came from and how it is checked.

- CLI build: `codex-0.155.0-alpha.9`
- Captured: 2026-09-20, read-only from this machine
- Source A: rollout session metadata `session_meta.payload.base_instructions.text`
  (`~/.codex/sessions/2026/09/20/rollout-2026-09-20T15-19-03-01a0bdae-c9c2-7ad3-82de-d1fd1a3c3e24.jsonl`)
- Source B: official catalog entry `route-mu944g8k-fjc2ct/gpt-6-astra.base_instructions`
  (`~/.codex/model-catalogs/codey-official.json`)

Both sources are byte-identical (21261 UTF-16 code units, LF line endings). The base template
digest below is the same value recorded in the Gemini instruction fingerprint test.

- Normalization: replace `CRLF` with `LF`, then trim surrounding whitespace
- SHA256 (normalized): `be213cc3a9566255f6d43f61c54cbc33cafbc3461680ad6513afa0850050c7d6`

The router only compares this normalized form against the exact stored template. A missing,
unknown, concatenated, or input-embedded template is still rejected before the request reaches
the upstream.

# The rules

Seven of them, and they are what the project is rather than how it is
organised. They have not changed since M0 except where a rule turned out to
describe something that never happened, which is noted in place.

1. **Assets are never committed.** Only format descriptions, hashes, offsets
   and code go into the repository. `.gitignore` is already set up — do not
   weaken it.
2. **No decompiled code in `engine/`.** Extract *behaviour* from the
   disassembler, describe it in prose in `docs/`, then implement it fresh.
   Ghidra pseudocode that comes up in conversation belongs in
   `docs/analysis/` as a reference and is never transcribed line by line into
   the engine.
3. **A format counts as solved at 100% coverage.** The parser must consume
   every file of that type in all three editions with no errors and no
   unexplained tail. 95% means the hypothesis is wrong, not that it almost
   worked.
4. **A format's description lives with the code that reads it**, and is
   written before the code that reads it twice. In practice that is the
   tool's module docstring — `tools/texdec.py` is the reference for the
   texture codec, `tools/mod2obj.py` for models — and the engine's matching
   module documentation, which must not disagree with it, and which carries
   the reasoning and the refutations along with it. *An earlier version of this
   rule named `docs/formats/*.ksy` as the source of truth; no `.ksy` was
   ever written, and the descriptions went where the code is instead.*
5. **No magic number without a comment** saying where it came from: the data,
   the disassembly, or a GL trace.
6. **Record hypotheses together with their refutations**, beside the code
   that settled them: what was tried, how it was tested, and what the result
   was. Dead ends are valuable — they save the work being repeated, and the
   docstrings here are full of them.
7. **Documents are written in English**, including the code comments.

What has been decided and what is still open is in the
[README](../README.md); why, and what was refuted on the way, is in the
docstring of whichever tool settled it.

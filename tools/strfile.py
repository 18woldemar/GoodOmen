#!/usr/bin/env python3
"""
strfile.py -- read `.str`: every line of text in the game, and its voice-over.

`override/<language>/mdk2.str` is where the translations live, and the GOG
release ships five of them, so no second edition is needed to find out what
localising this game touches. See below: it touches this file and nothing
else.

    0x00  u32   2003        resource type tag: .tex 2001, .mod 2002, .str 2003
    0x04  u32   2           version
    0x08  u32               entry count
    0x0c  u32   0x18        offset of the entry table, i.e. right here
    0x10  u32               offset of the character data
    0x14  u32               offset of the sound-name table

    entry, 12 bytes:  { u32 id; u32 text; u32 sound }

`text` is a byte offset into the character data and the string is a
NUL-terminated run of **16-bit units, each holding one code-page byte**.
`sound` is an offset into the sound-name table, whose entries are ASCII in
**fixed 16-byte slots**, naming a `.wav`. Either offset may be 0xFFFFFFFF:
47 entries have no text and 338 have no voice-over.

*Refuted hypothesis: the text is UTF-16LE.* It reads as UTF-16 and is not.
No unit in any of the six files known to us exceeds 0x00FF, and the English
file uses 0x91 and 0x92 -- C1 control characters in Unicode, and the curly
quotes in cp1252. French adds 0x9c, cp1252's oe. The engine widens a
code-page byte to 16 bits; it does not encode a code point. Latin-1 is the
first 256 code points, so for the five Western languages the wrong reading
gives the right answer, and only a sixth file settles it: the 1C Russian
edition stores cp1251, where 0xC4 is a capital De and UTF-16 says A-diaeresis.
That release also ships `font.tex` and `dialogfont.tex` in its `local.zip`,
which the GOG one does not -- the glyphs come with the translation, because
what a byte above 0x7f *means* is decided by the font and not by the file.

That pairing is the interesting part. The subtitle and the line that speaks it
are stored together, so `.str` is not only a UI-string table -- it is the
script of the game. Entry 1 is "Door's broken. " with the sound `jd_doors`,
and `local/` ships `jd_doors.wav` right beside this file.

Checked to the 100% rule on the GOG English data: 686 entries, every text
offset in range and every unit below 0x100, the strings tiling the character
data with no byte unaccounted for, and the 348 sound offsets landing exactly
on the 348 slots of a 5568-byte table -- 5568 = 348 x 16, and 16 bytes is the
whole remainder of the file.

**Six languages, and the sixth was needed.** The GOG release ships
`override/{english,french,german,italian,spanish}/mdk2.str` and the 1C
release a Russian one, and all six parse byte-exactly -- strings tiling the
character data with nothing left over, sound offsets landing on exactly the
slots that exist. Five of different sizes looked like enough to call the
format understood, and on the encoding they were not: see above.

They also settle what localising this game means, and here five *were* not
enough either -- the answer is this file **and the font**, which only a
release with a different alphabet shows. Of the text: **one file is the whole
of it.** 678 ids are common to all five Western tables; the
eight the English build has on top are entries with no text at all,
placeholders the translators dropped. Between 547 and 635 of the 678 strings
are genuinely translated -- "Black Hole Grenade" becomes "Grenade a Trou
Noir", "Black Hole Granate", "Granada aguj. negro".

The **voice-over is not translated with them**. The sound each line names is
the same in 672 of 678 for French and 671 for German; where they differ it is
almost always a line that has a recording in one language and none in
another, not a different recording. Only one entry in the whole set points at
a genuinely different file: Italian id 84 says `ml4z_shwang1a` where English
says `ml4z_shwang2`.

Usage:
    python3 tools/strfile.py extracted/local/mdk2.str
    python3 tools/strfile.py extracted/local/mdk2.str --json > strings.json
    python3 tools/strfile.py extracted/local/mdk2.str --grep health
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
from pathlib import Path

TYPE_STR = 2003
HEADER = 24
ENTRY = 12
SLOT = 16                 # the sound-name table's fixed record
ABSENT = 0xFFFFFFFF


class StrError(ValueError):
    pass


def _text(data: bytes, start: int, codepage: str) -> str:
    """One 16-bit unit per character, each holding one code-page byte."""
    end = start
    while data[end:end + 2] != b"\0\0":
        if end >= len(data):
            raise StrError(f"string at {start:#x} is unterminated")
        end += 2
    units = data[start:end]
    if units[1::2] != bytes(len(units) // 2):
        raise StrError(f"string at {start:#x} has a unit above 0xff, so it "
                       f"is not a widened code-page byte after all")
    return units[0::2].decode(codepage)


def parse(data: bytes, codepage: str = "cp1252") -> dict:
    tag, version, count, table, text_at, sound_at = struct.unpack_from(
        "<6I", data, 0)
    if tag != TYPE_STR:
        raise StrError(f"type tag {tag}, expected {TYPE_STR}")
    if table != HEADER or text_at != HEADER + count * ENTRY:
        raise StrError(f"table at {table:#x} and data at {text_at:#x} "
                       f"do not fit {count} entries")
    if sound_at > len(data) or (len(data) - sound_at) % SLOT:
        raise StrError(f"sound table at {sound_at:#x} is not a whole "
                       f"number of {SLOT}-byte slots")
    entries = {}
    for i in range(count):
        ident, text, sound = struct.unpack_from("<3I", data, table + i * ENTRY)
        entries[ident] = {
            "text": None if text == ABSENT else
                    _text(data, text_at + text, codepage),
            "sound": None if sound == ABSENT else
                     data[sound_at + sound:sound_at + sound + SLOT]
                         .split(b"\0")[0].decode("ascii"),
            "text_offset": None if text == ABSENT else text,
            "sound_offset": None if sound == ABSENT else sound,
        }
    return {"version": version, "text_at": text_at, "sound_at": sound_at,
            "entries": entries}


def coverage(data: bytes, table: dict) -> tuple[int, int, int, int]:
    """-> (text bytes used, text bytes present, sound slots used, present)."""
    used = sum(2 * len(e["text"]) + 2          # one unit per character, + NUL
               for e in table["entries"].values() if e["text"] is not None)
    slots = {e["sound_offset"] for e in table["entries"].values()
             if e["sound_offset"] is not None}
    return (used, table["sound_at"] - table["text_at"],
            len(slots), (len(data) - table["sound_at"]) // SLOT)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("src", type=Path)
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--grep", help="print only strings containing this")
    ap.add_argument("--compare", nargs="+", type=Path, metavar="STR",
                    help="compare other language tables against this one")
    ap.add_argument("--codepage", default="cp1252",
                    help="the code page the release's font draws; the five "
                         "GOG languages are cp1252, the 1C Russian cp1251")
    args = ap.parse_args(argv)

    data = args.src.read_bytes()
    table = parse(data, args.codepage)
    entries = table["entries"]

    if args.json:
        json.dump({str(k): {"text": v["text"], "sound": v["sound"]}
                   for k, v in sorted(entries.items())},
                  sys.stdout, indent=1, ensure_ascii=False)
        return 0

    if args.compare:
        others = {p.parent.name or p.stem:
                  parse(p.read_bytes(), args.codepage)["entries"]
                  for p in args.compare}
        common = set(entries)
        for t in others.values():
            common &= set(t)
        print(f"{len(common)} ids common to all "
              f"{len(others) + 1} tables; this one has {len(entries)}")
        for name, t in others.items():
            same_sound = sum(1 for k in common
                             if t[k]["sound"] == entries[k]["sound"])
            translated = sum(1 for k in common
                             if t[k]["text"] != entries[k]["text"])
            print(f"  {name:10s} {translated:4d} strings translated, "
                  f"{same_sound}/{len(common)} name the same sound")
        return 0

    needle = (args.grep or "").lower()
    for ident, e in sorted(entries.items()):
        if e["text"] is None:
            continue
        if needle and needle not in e["text"].lower():
            continue
        voice = f"  [{e['sound']}]" if e["sound"] else ""
        print(f"{ident:5d}  {e['text']!r}{voice}")

    text_used, text_have, slots_used, slots_have = coverage(data, table)
    spoken = sum(1 for e in entries.values() if e["sound"])
    print(f"{len(entries)} entries, {spoken} with a voice-over; "
          f"text {text_used}/{text_have} bytes, "
          f"sound slots {slots_used}/{slots_have}", file=sys.stderr)
    return 0 if (text_used, slots_used) == (text_have, slots_have) else 1


if __name__ == "__main__":
    raise SystemExit(main())

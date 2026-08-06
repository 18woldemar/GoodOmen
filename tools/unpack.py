#!/usr/bin/env python3
"""
unpack.py -- распаковка контейнеров data/*.zip.

Контейнеры MDK2 -- обычные ZIP, но метод сжатия 10 (PKWARE DCL Implode,
он же "implode"/"ibm-terse"). Его не умеют ни python-zipfile, ни unzip,
ни 7z, ни libarchive -- поэтому декомпрессор здесь свой.

Алгоритм -- blast (Mark Adler, zlib/contrib/blast). Таблицы Хаффмана
фиксированные, зашиты в формат, взяты из описания алгоритма.

Проверка корректности встроена в формат: CRC32 каждого файла лежит в
центральном каталоге ZIP, распаковщик его сверяет. Расхождение = ошибка.

Usage:
    python3 tools/unpack.py "$MDK2_GOG/data/base.zip" -o extracted/base
    python3 tools/unpack.py base.zip -o out '*.tex'      # только текстуры
    python3 tools/unpack.py base.zip --list
"""

from __future__ import annotations

import argparse
import binascii
import fnmatch
import struct
import sys
import zipfile
from pathlib import Path

# --- blast: PKWARE DCL Implode -------------------------------------------
# Компактное представление длин кодов: (повтор-1) << 4 | длина.
# Значения из blast.c -- это часть спецификации формата, не магия.
_LITLEN = bytes((
    11, 124, 8, 7, 28, 7, 188, 13, 76, 4, 10, 8, 12, 10, 12, 10, 8, 23, 8,
    9, 7, 6, 7, 8, 7, 6, 55, 8, 23, 24, 12, 11, 7, 9, 11, 12, 6, 7, 22, 5,
    7, 24, 6, 11, 9, 6, 7, 22, 7, 11, 38, 7, 9, 8, 25, 11, 8, 11, 9, 12,
    8, 12, 5, 38, 5, 38, 5, 11, 7, 5, 6, 21, 6, 10, 53, 8, 7, 24, 10, 27,
    44, 253, 253, 253, 252, 252, 252, 13, 12, 45, 12, 45, 12, 61, 12, 45,
    44, 173))
_LENLEN = bytes((2, 35, 36, 53, 38, 23))
_DISTLEN = bytes((2, 20, 53, 230, 247, 151, 248))

_BASE = (3, 2, 4, 5, 6, 7, 8, 9, 10, 12, 16, 24, 40, 72, 136, 264)
_EXTRA = (0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8)

_MAXBITS = 13
_END_LEN = 519  # длина 519 = маркер конца потока


def _construct(rep: bytes) -> tuple[list[int], list[int]]:
    """Разворачивает компактную таблицу длин в канонический код Хаффмана."""
    lengths: list[int] = []
    for b in rep:
        lengths += [b & 15] * ((b >> 4) + 1)
    count = [0] * (_MAXBITS + 2)
    for ln in lengths:
        count[ln] += 1
    offs = [0] * (_MAXBITS + 3)
    for ln in range(1, _MAXBITS + 1):
        offs[ln + 1] = offs[ln] + count[ln]
    symbol = [0] * len(lengths)
    for sym, ln in enumerate(lengths):
        if ln:
            symbol[offs[ln]] = sym
            offs[ln] += 1
    return count, symbol


_LITCODE = _construct(_LITLEN)
_LENCODE = _construct(_LENLEN)
_DISTCODE = _construct(_DISTLEN)


class _Bits:
    """Битовый поток, младший бит первым."""

    __slots__ = ("d", "i", "buf", "cnt")

    def __init__(self, data: bytes) -> None:
        self.d = data
        self.i = 0
        self.buf = 0
        self.cnt = 0

    def bits(self, need: int) -> int:
        val, cnt, i, d = self.buf, self.cnt, self.i, self.d
        while cnt < need:
            val |= d[i] << cnt
            i += 1
            cnt += 8
        self.buf, self.cnt, self.i = val >> need, cnt - need, i
        return val & ((1 << need) - 1)

    def decode(self, code_tbl: tuple[list[int], list[int]]) -> int:
        """Декодирует символ. Коды инвертированы -- особенность DCL."""
        count, symbol = code_tbl
        code = first = index = 0
        buf, cnt, i, d = self.buf, self.cnt, self.i, self.d
        for length in range(1, _MAXBITS + 1):
            if cnt == 0:
                buf = d[i]
                i += 1
                cnt = 8
            code |= (buf & 1) ^ 1
            buf >>= 1
            cnt -= 1
            c = count[length]
            if code < first + c:
                self.buf, self.cnt, self.i = buf, cnt, i
                return symbol[index + (code - first)]
            index += c
            first = (first + c) << 1
            code <<= 1
        raise ValueError("blast: недопустимый код Хаффмана")


def blast(data: bytes) -> bytes:
    """DCL Implode -> сырые байты."""
    b = _Bits(data)
    coded_literals = b.bits(8)
    if coded_literals > 1:
        raise ValueError(f"blast: неизвестный флаг литералов {coded_literals}")
    dict_bits = b.bits(8)
    if not 4 <= dict_bits <= 6:
        raise ValueError(f"blast: недопустимый размер словаря {dict_bits}")

    out = bytearray()
    while True:
        if b.bits(1):
            sym = b.decode(_LENCODE)
            length = _BASE[sym] + b.bits(_EXTRA[sym])
            if length == _END_LEN:
                return bytes(out)
            # для длины 2 расстояние всегда короткое: 2 бита вместо словарных
            dbits = 2 if length == 2 else dict_bits
            dist = (b.decode(_DISTCODE) << dbits) + b.bits(dbits) + 1
            if dist > len(out):
                raise ValueError("blast: ссылка за начало потока")
            start = len(out) - dist
            if dist >= length:
                out += out[start:start + length]
            else:  # перекрывающееся копирование, побайтно
                for k in range(length):
                    out.append(out[start + k])
        else:
            out.append(b.decode(_LITCODE) if coded_literals else b.bits(8))


# --- ZIP -----------------------------------------------------------------
_IMPLODE = 10
_LOCAL_HDR = struct.Struct("<4s5H3I2H")


def read_raw(zf: zipfile.ZipFile, info: zipfile.ZipInfo) -> bytes:
    """Сырые сжатые байты члена архива, минуя декомпрессоры zipfile."""
    fp = zf.fp
    fp.seek(info.header_offset)
    hdr = _LOCAL_HDR.unpack(fp.read(_LOCAL_HDR.size))
    if hdr[0] != b"PK\x03\x04":
        raise ValueError(f"{info.filename}: битый локальный заголовок")
    fp.seek(hdr[9] + hdr[10], 1)  # имя + extra
    return fp.read(info.compress_size)


def extract(zf: zipfile.ZipFile, info: zipfile.ZipInfo) -> bytes:
    if info.compress_type == _IMPLODE:
        data = blast(read_raw(zf, info))
    else:
        data = zf.read(info)
    if len(data) != info.file_size:
        raise ValueError(
            f"{info.filename}: размер {len(data)} != {info.file_size}")
    if binascii.crc32(data) != info.CRC:
        raise ValueError(f"{info.filename}: CRC32 не сошёлся")
    return data


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("archive", type=Path)
    ap.add_argument("patterns", nargs="*", help="глоб-маски, по умолчанию всё")
    ap.add_argument("-o", "--out", type=Path, help="каталог назначения")
    ap.add_argument("--list", action="store_true", help="только перечислить")
    args = ap.parse_args(argv)

    with zipfile.ZipFile(args.archive) as zf:
        members = zf.infolist()
        if args.patterns:
            members = [m for m in members if any(
                fnmatch.fnmatch(m.filename.lower(), p.lower())
                for p in args.patterns)]
        if args.list:
            for m in members:
                print(f"{m.file_size:10d}  {m.compress_type:2d}  {m.filename}")
            print(f"-- {len(members)} файлов", file=sys.stderr)
            return 0
        if not args.out:
            ap.error("нужен -o OUT либо --list")
        args.out.mkdir(parents=True, exist_ok=True)

        bad = 0
        for n, m in enumerate(members, 1):
            try:
                data = extract(zf, m)
            except ValueError as e:
                print(f"ОШИБКА {e}", file=sys.stderr)
                bad += 1
                continue
            # имена внутри контейнеров плоские, но защита от .. не лишняя
            dest = args.out / Path(m.filename).name
            dest.write_bytes(data)
            if n % 200 == 0:
                print(f"  {n}/{len(members)}", file=sys.stderr)
        print(f"{len(members) - bad}/{len(members)} файлов в {args.out}",
              file=sys.stderr)
        return 1 if bad else 0


def _selfcheck() -> None:
    """Прогоняет несколько членов реального контейнера; CRC32 -- эталон."""
    import os
    root = os.environ.get("MDK2_GOG")
    assert root, "нужен MDK2_GOG в окружении"
    with zipfile.ZipFile(Path(root) / "data" / "base.zip") as zf:
        members = zf.infolist()
        assert any(m.compress_type == _IMPLODE for m in members)
        for m in members[:20] + members[-5:]:
            extract(zf, m)  # бросит ValueError при расхождении CRC
    print("selfcheck ok")


if __name__ == "__main__":
    if "--selfcheck" in sys.argv:
        _selfcheck()
    else:
        raise SystemExit(main())

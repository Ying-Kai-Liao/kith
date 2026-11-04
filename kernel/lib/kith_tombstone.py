"""kith_tombstone: how an old Python reader finds out that a module's file moved to the Kith core.

When `kith migrate step MODULE` switches a module, the files it read (auth.json, state.json ...) are replaced by a tombstone:
a JSON object with "kith_tombstone": 1, the module's name and the way back (`kith rollback MODULE`). The format is written by
core/runtime/src/cutover/tombstone.rs. A reader that just json.load()s such a file would see an object with none of its keys
and carry on as if the data were empty, which is the failure this closes: call one of these first and it refuses.

  import kith_tombstone
  text = kith_tombstone.read_text(path)        # the file's text, or Tombstoned
  data = kith_tombstone.load_json(path)        # json.load, or Tombstoned
  kith_tombstone.check(path)                   # raises Tombstoned when path holds a tombstone, else returns

  kith_tombstone.py PATH...                    # exit 0 when none is a tombstone, 3 (one line on stderr) when one is

Tombstoned is an OSError, so a reader's existing "cannot read this file" handling fails closed too. A missing file is not a
tombstone (the caller's own rule for a missing file stays). Wired into no reader yet; each module's switch wires its own.
"""
import json
import sys


class Tombstoned(OSError):
    def __init__(self, path, module, rollback):
        self.path, self.module = str(path), module
        super().__init__(f'{path} moved to the Kith core (module {module}); this reader must not use it ({rollback})')


def _note(text):
    """The tombstone object in `text`, or None."""
    if '"kith_tombstone"' not in text:
        return None
    try:
        v = json.loads(text)
    except ValueError:
        return None
    return v if isinstance(v, dict) and 'kith_tombstone' in v else None


def check_text(path, text):
    n = _note(text)
    if n is not None:
        raise Tombstoned(path, n.get('module', '?'), n.get('rollback') or 'kith rollback ' + str(n.get('module', '')))


def check(path):
    try:
        with open(path, encoding='utf-8') as f:
            text = f.read(65536)
    except (OSError, UnicodeDecodeError):
        return   # missing or unreadable: the caller's own rule
    check_text(path, text)


def read_text(path, encoding='utf-8'):
    with open(path, encoding=encoding) as f:
        text = f.read()
    check_text(path, text)
    return text


def load_json(path):
    return json.loads(read_text(path))


def main(argv):
    for p in argv:
        try:
            check(p)
        except Tombstoned as e:
            print(f'kith: {e}', file=sys.stderr)
            return 3
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

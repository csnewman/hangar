#!/usr/bin/env python3
"""Writes the desktop app's spelling dictionaries from the web's packages.

The web checks spelling against Hunspell's en_US (dictionary-en) and
cspell's lists of software's words; the app bundles the same: en_US's
affix and word files as they are, and the cspell lists as one file of
words, lower-cased, without comments, forbidden words (!) or patterns and
compounds (* and +), as the web's spellWorker.ts reads them. Run from the
repository with the web's packages installed (npm ci in web/).
"""
import gzip
import pathlib
import shutil

repo = pathlib.Path(__file__).resolve().parent.parent
modules = repo / "web" / "node_modules"
out = repo / "crates" / "app" / "assets" / "spell"

# The lists spellWorker.ts loads, in its order.
LISTS = """
dict-software-terms/dict/softwareTerms.txt.gz
dict-software-terms/dict/software-tools.txt
dict-software-terms/dict/networkingTerms.txt
dict-software-terms/dict/webServices.txt
dict-software-terms/dict/computing-acronyms.txt
dict-software-terms/dict/posix-errno.txt
dict-software-terms/dict/posix-signals.txt
dict-aws/dict/aws.txt
dict-companies/dict/companies.txt
dict-css/dict/css.txt
dict-docker/dict/docker-words.txt.gz
dict-dotnet/dict/dotnet.txt
dict-filetypes/filetypes.txt.gz
dict-fullstack/dict/fullstack.txt
dict-golang/dict/go.txt
dict-html/dict/html.txt.gz
dict-k8s/dict/k8s.txt
dict-lua/dict/lua.txt
dict-node/dict/node.txt
dict-npm/dict/npm.txt
dict-php/dict/php.txt
dict-python/dict/python.txt
dict-python/dict/python-common.txt
dict-ruby/dict/ruby.txt
dict-rust/dict/rust.txt.gz
dict-shell/dict/shell-all-words.txt
dict-sql/sql.txt.gz
dict-typescript/dict/typescript.txt
""".split()


def read(path: pathlib.Path) -> str:
    data = path.read_bytes()
    if data[:2] == b"\x1f\x8b":
        data = gzip.decompress(data)
    return data.decode("utf-8")


def main() -> None:
    out.mkdir(parents=True, exist_ok=True)
    en = modules / "dictionary-en"
    shutil.copyfile(en / "index.aff", out / "en_US.aff")
    shutil.copyfile(en / "index.dic", out / "en_US.dic")
    shutil.copyfile(en / "license", out / "LICENSE-en_US")
    words = set()
    for name in LISTS:
        for line in read(modules / "@cspell" / name).splitlines():
            w = line.strip().removeprefix("~")
            if not w or w.startswith("#") or any(c in w for c in "!*+ \t"):
                continue
            words.add(w.lower())
    (out / "software.txt").write_text("".join(w + "\n" for w in sorted(words)))
    shutil.copyfile(modules / "@cspell" / "dict-software-terms" / "LICENSE", out / "LICENSE-cspell")


if __name__ == "__main__":
    main()

"""Bundle the local corpus into a standalone annotation page; no server needed."""

import argparse
import base64
import json
from pathlib import Path


def main():
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=root / ".cache/vad-annotator-ready.html")
    args = parser.parse_args()
    corpus = root / "assets/eval"
    manifest = json.loads((corpus / "manifest.json").read_text(encoding="utf-8"))
    sources = json.loads((corpus / "sources.json").read_text(encoding="utf-8"))
    wavs = {}
    for fixture in manifest["fixtures"]:
        name = fixture["wav"]
        if not name or any(c in name for c in "\\/:") or name in (".", ".."):
            raise ValueError(f"Expected local WAV filename: {name!r}")
        audio = (corpus / name).read_bytes()
        if len(audio) > 960000 + 65536:
            raise ValueError(f"WAV exceeds annotation limit: {name}")
        wavs[name] = base64.b64encode(audio).decode("ascii")
    payload = json.dumps({"manifest": manifest, "sources": sources, "wavs": wavs}, ensure_ascii=True)
    # Keep data inert even if a transcript contains an HTML closing script tag.
    payload = payload.replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    template = (root / "tools/vad-annotator.html").read_text(encoding="utf-8")
    marker = "<script>"
    if template.count(marker) != 1:
        raise ValueError("Expected exactly one annotation script")
    bundled = template.replace(marker, '<script id="bundledCorpus" type="application/json">' + payload + "</script>\n" + marker, 1)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(bundled, encoding="utf-8")
    print(f"Ready: {args.output.resolve()} ({len(manifest['fixtures'])} clips)")


if __name__ == "__main__":
    main()

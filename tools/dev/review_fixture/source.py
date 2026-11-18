"""The small source observations frozen by the worked manifest."""

from pathlib import Path

from .bundle import encode_json, sha256


REVIEW_SOURCES = Path(__file__).resolve().parents[3] / "fixtures" / "reviews"
WORKED = REVIEW_SOURCES / "worked"
STAMP = "2026-11-01T00:00:00Z"


def worked_files():
    files = {
        name: (WORKED / name).read_bytes()
        for name in ("reviews.json", "sessions.json", "discussions.json", "timeline.json")
    }
    for name, count in (("quiet", 20), ("active", 1200), ("large", 12000)):
        lines = [f"@@ -0,0 +1,{count} @@"]
        lines.extend(f"+export const example{i} = {i};" for i in range(count))
        files[f"{name}.patch"] = "\n".join(lines).encode("utf-8")

    entries = [
        {
            "lead": "active",
            "id": f"active-item-{i}",
            "turn": f"active-turn-{i}",
            "text": f"Update {i} " + "Visible conversation text. " * 10,
            "observed_at": STAMP,
        }
        for i in range(100)
    ]
    entries.append(
        {
            "lead": "quiet",
            "id": "quiet-item-0",
            "turn": "quiet-turn-0",
            "text": "Quiet complete",
            "observed_at": STAMP,
        }
    )
    files["history.json"] = encode_json(entries)

    manifest = encode_json(
        {
            "version": 1,
            "outbox": [],
            "files": {
                name: {"bytes": len(data), "sha256": sha256(data)}
                for name, data in files.items()
            },
        }
    )
    if manifest != (WORKED / "manifest.json").read_bytes():
        raise ValueError("worked source bytes differ from fixtures/reviews/worked/manifest.json")
    files["manifest.json"] = manifest
    return files
